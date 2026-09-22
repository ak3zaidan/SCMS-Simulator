//! The bindings driven from Rust, with a real interpreter.
//!
//! Every test here runs Python. `cargo test -p v2xw-py` links libpython (the
//! `extension-module` feature is deliberately not a default, see the manifest), so these
//! need no wheel and no `maturin`.
//!
//! # What each test is for
//!
//! The plug-in tests come in pairs: one shows a well-behaved plug-in passing a check, and
//! one **injects the fault the check exists for** and shows the same check going red. A
//! conformance check that has never been observed to fail is not a check.

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Starts the interpreter once, then builds the module.
///
/// `prepare_freethreaded_python` is idempotent and safe to call from several tests at once;
/// the module itself is built fresh per call, which costs nothing and keeps one test's
/// namespace out of another's.
fn module(py: Python<'_>) -> Bound<'_, PyModule> {
    let m = _v2xw::build_module(py).expect("the native module builds");
    // A plug-in's source says `import _v2xw`, as a researcher's would. The wheel puts the
    // module there; a standalone build has to.
    py.import("sys")
        .and_then(|s| s.getattr("modules"))
        .and_then(|mods| mods.set_item("_v2xw", &m))
        .expect("the module registers in sys.modules");
    m
}

/// Ensures the interpreter is running before any `Python::with_gil`.
fn interpreter() {
    pyo3::prepare_freethreaded_python();
}

/// A minimal scenario loads, validates and hashes.
#[test]
fn a_minimal_scenario_loads_and_hashes() {
    interpreter();
    Python::with_gil(|py| {
        let m = module(py);
        let scenario = m
            .getattr("Scenario")
            .unwrap()
            .call_method0("minimal")
            .unwrap();
        let problems: Vec<String> = scenario
            .call_method0("problems")
            .unwrap()
            .extract()
            .unwrap();
        assert!(
            problems.is_empty(),
            "the minimal scenario must validate, got {problems:?}"
        );
        let hash: String = scenario.getattr("content_hash").unwrap().extract().unwrap();
        assert_eq!(hash.len(), 64, "a SHA-256 in hex is 64 characters");
        let again: String = m
            .getattr("Scenario")
            .unwrap()
            .call_method0("minimal")
            .unwrap()
            .getattr("content_hash")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            hash, again,
            "the content hash is a function of the document"
        );
    });
}

/// `v2xw.math` is the engine's libm, not the interpreter's.
///
/// The values are compared bit for bit against `v2xw_core::math`, which is the whole point:
/// if the binding routed to CPython's `math` instead, this would pass on the machine that
/// wrote it and fail on another, so the comparison is against the Rust function and not
/// against a literal.
#[test]
fn the_math_module_is_the_engines_libm() {
    interpreter();
    Python::with_gil(|py| {
        let m = module(py).getattr("math").unwrap();
        for x in [0.5_f64, 1.0, 2.0, 10.0, 1e-8, 123.456] {
            let got: f64 = m.call_method1("exp", (x,)).unwrap().extract().unwrap();
            assert_eq!(
                got.to_bits(),
                v2xw_core::math::exp(x).to_bits(),
                "exp({x}) must be the engine's, bit for bit"
            );
            let got: f64 = m.call_method1("ln", (x,)).unwrap().extract().unwrap();
            assert_eq!(got.to_bits(), v2xw_core::math::ln(x).to_bits());
        }
        let got: f64 = m
            .call_method1("pow", (1.5, 4.0))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(got.to_bits(), v2xw_core::math::pow(1.5, 4.0).to_bits());

        // Quantisation: the writer-side rounding of build decision D9, and its check.
        let q: f64 = m
            .call_method1("quantize_to", (1.23456789, 1e-3))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            q.to_bits(),
            v2xw_core::math::quantize_to(1.23456789, 1e-3).to_bits()
        );
        let on: bool = m
            .call_method1("is_on_grid", (q, 1e-3))
            .unwrap()
            .extract()
            .unwrap();
        assert!(on, "a quantised value sits on its grid");
        let on: bool = m
            .call_method1("is_on_grid", (1.23456789, 1e-3))
            .unwrap()
            .extract()
            .unwrap();
        assert!(!on, "an unquantised value does not");
    });
}

/// `sum_ordered` does not depend on the order it was handed.
#[test]
fn ordered_summation_is_order_independent() {
    interpreter();
    Python::with_gil(|py| {
        let m = module(py).getattr("math").unwrap();
        let ascending = vec![1e-16_f64, 1.0, 1e16, 2.0, -1e16];
        let mut descending = ascending.clone();
        descending.reverse();
        let a: f64 = m
            .call_method1("sum_ordered", (ascending,))
            .unwrap()
            .extract()
            .unwrap();
        let b: f64 = m
            .call_method1("sum_ordered", (descending,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "a reduction that reaches an output may not depend on the input's order"
        );
    });
}

/// The source of a Python IDM, as 03-interfaces.md §15 has a researcher write one.
///
/// It uses `v2xw.math` rather than `math` and `**`, which is rule 3 of the plug-in
/// contract. The parameters come off the ego's driver profile, which is where the engine
/// puts the per-driver draw, so the model itself holds no state and reads no defaults of
/// its own.
const IDM_SOURCE: &str = r#"
import _v2xw
m = _v2xw.math

CARD = {
    "id": "mobility/car-following/python-idm",
    "family": "mobility",
    "version": "0.1.0",
    "api_version": "1.0.0",
    "tier": ["medium"],
    "purpose": "Treiber's Intelligent Driver Model, written in Python.",
    "equations": [],
    "parameters": [],
    "determinism": {"uses_rng": False},
}

class Idm:
    card = CARD
    def accel(self, ego, leader, lane, weather):
        v0 = min(ego.desired_speed_mps, lane.speed_limit_mps)
        a = ego.max_accel_mps2
        free = a * (1.0 - m.pow(ego.speed_mps / v0, 4.0)) if v0 > 0.0 else -a
        if leader is None:
            return free
        dv = ego.speed_mps - leader.speed_mps
        s_star = (ego.min_gap_m
                  + ego.speed_mps * ego.time_headway_s
                  + ego.speed_mps * dv / (2.0 * m.sqrt(a * ego.comfort_decel_mps2)))
        s = max(leader.gap_m, 0.01)
        return free - a * m.pow(max(s_star, 0.0) / s, 2.0)
"#;

/// Builds an object from `source` by executing it and taking `name` out of the namespace.
fn build<'py>(py: Python<'py>, source: &str, name: &str) -> Bound<'py, PyAny> {
    let _ = module(py);
    let globals = PyDict::new(py);
    let code = std::ffi::CString::new(source).unwrap();
    py.run(&code, Some(&globals), None).unwrap_or_else(|e| {
        e.print(py);
        panic!("the plug-in source must execute");
    });
    globals.get_item(name).unwrap().unwrap().call0().unwrap()
}

/// Attaches a handle of `class_name` from `source`.
fn attach<'py>(py: Python<'py>, source: &str, class_name: &str, kind: &str) -> Bound<'py, PyAny> {
    let object = build(py, source, class_name);
    module(py)
        .getattr("plugins")
        .unwrap()
        .getattr(kind)
        .unwrap()
        .call1((object,))
        .unwrap_or_else(|e| {
            e.print(py);
            panic!("the plug-in must attach");
        })
}

/// A Python car-following model is called through the engine's trait and passes the kit.
#[test]
fn a_python_car_following_model_passes_conformance() {
    interpreter();
    Python::with_gil(|py| {
        let handle = attach(py, IDM_SOURCE, "Idm", "CarFollowingModel");
        let kit = module(py).getattr("_conformance").unwrap();

        for check in [
            "check_determinism",
            "check_thread_independence",
            "check_finite",
        ] {
            let outcome = kit.call_method1(check, (&handle,)).unwrap();
            let passed: bool = outcome.get_item("passed").unwrap().extract().unwrap();
            let detail: String = outcome.get_item("detail").unwrap().extract().unwrap();
            assert!(passed, "{check} failed on a well-behaved plug-in: {detail}");
        }

        // A plausible number, and the right sign: a car at its desired speed on a free road
        // neither accelerates hard nor brakes.
        let free: f64 = handle
            .call_method1("accel", (13.888_889_f64,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(
            free.abs() < 0.5,
            "a car at the speed limit on a free road should be near zero acceleration, got {free}"
        );
        // Two metres behind a stopped car: hard braking.
        let kwargs = PyDict::new(py);
        kwargs.set_item("gap_m", 2.0).unwrap();
        kwargs.set_item("leader_speed_mps", 0.0).unwrap();
        let emergency: f64 = handle
            .call_method("accel", (13.888_889_f64,), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert!(
            emergency < -3.0,
            "two metres behind a stopped car must brake hard, got {emergency}"
        );
    });
}

/// **The injected fault.** A plug-in that draws a random number fails the determinism
/// check.
///
/// This is the test that makes the determinism check real. The model is the IDM above with
/// one line added — a `random.gauss` on the acceleration, which is exactly the mistake §15
/// rule P1 warns about, and exactly the mistake that produces a run nobody can reproduce.
#[test]
fn a_plug_in_that_draws_random_numbers_fails_the_determinism_check() {
    let source = format!(
        "{IDM_SOURCE}
import random

class Sloppy(Idm):
    def accel(self, ego, leader, lane, weather):
        # The fault: a stream the engine does not know about, seeded from the OS.
        return Idm.accel(self, ego, leader, lane, weather) + random.gauss(0.0, 0.1)
"
    );
    interpreter();
    Python::with_gil(|py| {
        let handle = attach(py, &source, "Sloppy", "CarFollowingModel");
        let outcome = module(py)
            .getattr("_conformance")
            .unwrap()
            .call_method1("check_determinism", (&handle,))
            .unwrap();
        let passed: bool = outcome.get_item("passed").unwrap().extract().unwrap();
        let detail: String = outcome.get_item("detail").unwrap().extract().unwrap();
        assert!(
            !passed,
            "the determinism check must fail a plug-in that draws random numbers"
        );
        assert!(
            detail.contains("disagreed"),
            "the failure must say what disagreed, got {detail:?}"
        );
    });
}

/// **The injected fault.** A plug-in whose call raises produces a `NaN`, not a zero, and
/// the finiteness check catches it with the traceback.
#[test]
fn a_raising_plug_in_yields_nan_and_keeps_the_traceback() {
    let source = format!(
        "{IDM_SOURCE}

class Broken(Idm):
    def accel(self, ego, leader, lane, weather):
        raise ValueError('the gap is not what I expected')
"
    );
    interpreter();
    Python::with_gil(|py| {
        let handle = attach(py, &source, "Broken", "CarFollowingModel");
        let a: f64 = handle
            .call_method1("accel", (10.0_f64,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(
            a.is_nan(),
            "a raised exception must become a NaN, not a plausible acceleration; got {a}"
        );
        let err: Option<String> = handle.getattr("last_error").unwrap().extract().unwrap();
        let err = err.expect("the traceback is kept");
        assert!(
            err.contains("the gap is not what I expected"),
            "the stored error must be the plug-in's own, got {err:?}"
        );

        let outcome = module(py)
            .getattr("_conformance")
            .unwrap()
            .call_method1("check_finite", (&handle,))
            .unwrap();
        let passed: bool = outcome.get_item("passed").unwrap().extract().unwrap();
        assert!(!passed, "the finiteness check must fail a raising plug-in");
    });
}

/// A plug-in with no model card cannot attach at all.
#[test]
fn a_plug_in_without_a_card_is_refused() {
    interpreter();
    Python::with_gil(|py| {
        let object = build(
            py,
            "class NoCard:
    def accel(self, ego, leader, lane, weather):
        return 0.0
",
            "NoCard",
        );
        let e = module(py)
            .getattr("plugins")
            .unwrap()
            .getattr("CarFollowingModel")
            .unwrap()
            .call1((object,))
            .expect_err("a plug-in without a card must be refused");
        let message = e.to_string();
        assert!(
            message.contains("model card"),
            "the refusal must say what is missing, got {message:?}"
        );
    });
}

/// A card whose family is the wrong seam is refused, with both families named.
#[test]
fn a_card_for_the_wrong_family_is_refused() {
    let source = IDM_SOURCE.replace("\"family\": \"mobility\"", "\"family\": \"detector\"");
    interpreter();
    Python::with_gil(|py| {
        let object = build(py, &source, "Idm");
        let e = module(py)
            .getattr("plugins")
            .unwrap()
            .getattr("CarFollowingModel")
            .unwrap()
            .call1((object,))
            .expect_err("a detector may not attach as a car-following model");
        let message = e.to_string();
        assert!(
            message.contains("detector") && message.contains("mobility"),
            "the refusal must name both families, got {message:?}"
        );
    });
}

/// The card checks report an uncalibrated parameter that has no calibration plan.
///
/// Registry rule R1 refuses such a card outright, so the case this exercises is the one
/// the rule allows: a `todo-calibrate` default *with* a plan, which must not be reported.
#[test]
fn the_card_checks_distinguish_a_plan_from_its_absence() {
    interpreter();
    Python::with_gil(|py| {
        let plugins = module(py).getattr("plugins").unwrap();
        let with_plan = r#"{
            "id": "detect/local/threshold", "family": "detector", "version": "0.1.0",
            "api_version": "1.0.0", "tier": ["medium"], "purpose": "A threshold.",
            "equations": [],
            "parameters": [{"name": "limit", "unit": "m/s", "default": 90.0,
                            "source": {"kind": "todo-calibrate", "ref": "not fitted"},
                            "calibration": "Fit to the benign speed distribution."}]
        }"#;
        let uncited: Vec<String> = plugins
            .call_method1("uncited_parameters", (with_plan,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(
            uncited.is_empty(),
            "a todo-calibrate default with a plan is allowed, got {uncited:?}"
        );

        let without_plan = with_plan.replace(
            ",\n                            \"calibration\": \"Fit to the benign speed distribution.\"",
            "",
        );
        let uncited: Vec<String> = plugins
            .call_method1("uncited_parameters", (without_plan,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            uncited,
            vec!["limit".to_string()],
            "a todo-calibrate default with no plan must be reported"
        );
    });
}

/// An `Observation`'s confidence is quantised at construction, and an impossible one is
/// refused.
#[test]
fn an_observation_quantises_its_confidence() {
    interpreter();
    Python::with_gil(|py| {
        let plugins = module(py).getattr("plugins").unwrap();
        let obs = plugins
            .getattr("Observation")
            .unwrap()
            .call1((7_u32, "position-plausibility", 0.123_456_789_f64, 1_000_u64))
            .unwrap();
        let c: f64 = obs.getattr("confidence").unwrap().extract().unwrap();
        assert!(
            v2xw_core::math::is_on_grid(c, 1e-6),
            "confidence must land on the probability grid, got {c}"
        );
        assert_eq!(c, 0.123_457);

        for bad in [-0.1_f64, 1.5, f64::NAN] {
            plugins
                .getattr("Observation")
                .unwrap()
                .call1((7_u32, "k", bad, 0_u64))
                .expect_err("a confidence outside [0, 1] must be refused");
        }
    });
}

/// The Rust reference detector fires on an implausible speed and not on a plausible one.
///
/// It is driven through `dyn Detector` with a batch built in Rust, so the detector seam is
/// exercised whether or not `pyarrow` is installed. The Python detector's own round trip is
/// covered by `python/tests`, which run under a wheel that has `pyarrow`.
#[test]
fn the_reference_detector_reads_an_arrow_batch() {
    use _v2xw::plugins::{Detector, DetectorCtx, SpeedPlausibilityDetector};
    use arrow::array::{Float64Array, RecordBatch, UInt32Array};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    let schema = Arc::new(Schema::new(vec![
        Field::new("node", DataType::UInt32, false),
        Field::new("speed_mps", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt32Array::from(vec![1_u32, 2, 3])),
            Arc::new(Float64Array::from(vec![13.9_f64, 300.0, 25.0])),
        ],
    )
    .unwrap();

    let detector = SpeedPlausibilityDetector::default();
    let model: &dyn Detector = &detector;
    let out = model.on_messages(
        DetectorCtx {
            t_ns: 5_000,
            node: 9,
        },
        &batch,
    );
    assert_eq!(out.len(), 1, "only the 300 m/s claim is implausible");
    assert_eq!(out[0].subject, 2);
    assert_eq!(out[0].kind, "speed-plausibility");
    assert_eq!(out[0].t_ns, 5_000);

    // A batch without the column the detector reads yields nothing, rather than a zero.
    let empty = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new(
            "node",
            DataType::UInt32,
            false,
        )])),
        vec![Arc::new(UInt32Array::from(vec![1_u32]))],
    )
    .unwrap();
    assert!(
        model
            .on_messages(DetectorCtx { t_ns: 0, node: 0 }, &empty)
            .is_empty()
    );
}

/// The reference detector's card validates, which is what the registry requires of it.
#[test]
fn the_reference_detectors_card_validates() {
    use _v2xw::plugins::SpeedPlausibilityDetector;
    use v2xw_core::model::Model;
    let d = SpeedPlausibilityDetector::default();
    d.card()
        .validate()
        .expect("the reference detector's card must validate");
    assert_eq!(d.card().parameters.len(), 1);
    assert!(
        d.card().parameters[0].calibration.is_some(),
        "an uncalibrated default must carry its calibration plan (registry rule R1)"
    );
}
