//! One run, from a scenario to a metric and back out of the recording.
//!
//! This is the path the worked example takes, asserted rather than demonstrated. It is a
//! separate test binary from `bindings.rs` because it is the slow one: it builds a world
//! and runs the event loop, where the others only call a function.

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// The manifest timestamp. A fixed string, not a clock read: the engine excludes this field
/// from every digest precisely so a test can pin it, and a test that read the wall clock
/// would be the one place in the tree that did.
const BUILD_UTC: &str = "2026-09-22T00:00:00Z";

fn module(py: Python<'_>) -> Bound<'_, PyModule> {
    _v2xw::build_module(py).expect("the native module builds")
}

/// A short run of the minimal scenario, with the duration cut down to keep the test quick.
fn short_scenario<'py>(py: Python<'py>, seconds: f64) -> Bound<'py, PyAny> {
    let m = module(py);
    let scenario_cls = m.getattr("Scenario").unwrap();
    let base = scenario_cls.call_method0("minimal").unwrap();
    let doc = base.call_method0("as_dict").unwrap();
    doc.get_item("time")
        .unwrap()
        .set_item("duration_s", seconds)
        .unwrap();
    scenario_cls.call_method1("from_dict", (doc,)).unwrap()
}

/// A run produces a manifest, counters and metric samples, and writes a readable recording.
#[test]
fn a_run_produces_a_manifest_metrics_and_a_readable_recording() {
    pyo3::prepare_freethreaded_python();
    let dir = std::env::temp_dir().join(format!("v2xw-py-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("run.mcap");

    Python::with_gil(|py| {
        let m = module(py);
        let scenario = short_scenario(py, 2.0);

        let kwargs = PyDict::new(py);
        kwargs.set_item("build_utc", BUILD_UTC).unwrap();
        kwargs
            .set_item("recording", path.display().to_string())
            .unwrap();
        kwargs.set_item("metric_window_s", 0.5).unwrap();
        let run = m
            .call_method("run", (&scenario,), Some(&kwargs))
            .unwrap_or_else(|e| {
                e.print(py);
                panic!("the run must complete");
            });

        // The manifest identifies the run by the scenario it came from.
        let manifest = run.getattr("manifest").unwrap();
        let scenario_hash: String = manifest
            .get_item("scenario_hash")
            .unwrap()
            .extract()
            .unwrap();
        let expected: String = scenario.getattr("content_hash").unwrap().extract().unwrap();
        assert_eq!(
            scenario_hash, expected,
            "the manifest must pin the scenario that was run"
        );
        let build_utc: String = manifest.get_item("build_utc").unwrap().extract().unwrap();
        assert_eq!(build_utc, BUILD_UTC, "the timestamp is the caller's");

        // The loop ran to its horizon.
        let end_ns: u64 = run.getattr("end_ns").unwrap().extract().unwrap();
        assert!(
            end_ns > 0,
            "a two-second run must advance the clock, got {end_ns} ns"
        );

        // Nothing the engine emitted was refused by the recording. A non-zero count here
        // means the file is not the whole run, which would make every assertion below it
        // about a different run than the one that happened.
        let refused: u64 = run.getattr("records_refused").unwrap().extract().unwrap();
        assert_eq!(refused, 0, "the recording refused {refused} records");

        // Metrics came out, as a table and as an Arrow IPC buffer.
        let metrics = run.getattr("metrics").unwrap();
        let names: Vec<String> = metrics.call_method0("names").unwrap().extract().unwrap();
        assert!(
            !names.is_empty(),
            "a run with providers registered must produce at least one metric"
        );
        let ipc: Vec<u8> = metrics.call_method0("ipc").unwrap().extract().unwrap();
        assert!(
            ipc.starts_with(b"\xff\xff\xff\xff"),
            "an Arrow IPC stream begins with a continuation marker"
        );
        let digest: String = metrics.getattr("digest").unwrap().extract().unwrap();
        assert_eq!(digest.len(), 64);

        // The recording opens, verifies and hands its records back.
        let recording = m
            .getattr("Recording")
            .unwrap()
            .call_method1("open", (path.display().to_string(),))
            .unwrap();
        let report = recording.call_method0("verify").unwrap();
        let records: u64 = report.get_item("records").unwrap().extract().unwrap();
        let run_records: u64 = run.getattr("records").unwrap().extract().unwrap();
        assert_eq!(
            records, run_records,
            "the recording must hold every record the run emitted"
        );
        assert!(
            report
                .get_item("integrity_verified")
                .unwrap()
                .extract::<bool>()
                .unwrap(),
            "every chunk must carry a checksum that matched"
        );

        let manifest_back = recording.getattr("manifest").unwrap();
        let hash_back: String = manifest_back
            .get_item("scenario_hash")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            hash_back, expected,
            "the recording carries the run's own manifest"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// Two runs of one scenario produce identical record streams.
///
/// The determinism gate of ADR 0004, through the binding a researcher would use.
#[test]
fn two_runs_of_one_scenario_agree() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let m = module(py);
        let scenario = short_scenario(py, 1.0);
        let kwargs = PyDict::new(py);
        kwargs.set_item("build_utc", BUILD_UTC).unwrap();
        let (a, b): (String, String) = m
            .call_method("run_twice", (&scenario,), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(a, b, "two runs of one scenario must produce one digest");
        assert_eq!(a.len(), 64);
    });
}

/// A run with no recording and no metrics still reports what it did.
#[test]
fn a_run_can_write_nothing() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let m = module(py);
        let scenario = short_scenario(py, 1.0);
        let kwargs = PyDict::new(py);
        kwargs.set_item("build_utc", BUILD_UTC).unwrap();
        kwargs.set_item("metrics", false).unwrap();
        let run = m.call_method("run", (&scenario,), Some(&kwargs)).unwrap();
        assert!(run.getattr("recording").unwrap().is_none());
        let metrics = run.getattr("metrics").unwrap();
        assert_eq!(metrics.len().unwrap(), 0);
    });
}

/// `metric_window_s` must be a sane number, and says so when it is not.
#[test]
fn an_impossible_metric_window_is_refused() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let m = module(py);
        let scenario = short_scenario(py, 1.0);
        for bad in [-1.0_f64, f64::NAN] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("build_utc", BUILD_UTC).unwrap();
            kwargs.set_item("metric_window_s", bad).unwrap();
            let e = m
                .call_method("run", (&scenario,), Some(&kwargs))
                .expect_err("a nonsensical window must be refused");
            assert!(e.to_string().contains("metric_window_s"));
        }
    });
}
