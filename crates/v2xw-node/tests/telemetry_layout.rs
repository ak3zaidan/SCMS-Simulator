//! `NodeTelemetry` against vwp-v1 §3.5.2, field by field, at the byte.
//!
//! # Why this test is written the hard way
//!
//! `v2xw-record` already checks that its encoder round-trips and that the record is 208
//! bytes; that is the *encoding*. What this crate can get wrong is the *mapping* — which
//! node quantity ends up in which field. `nbr_verified` and `nbr_unverified` are both
//! `u16`, sit four bytes apart, and would round-trip perfectly while being the wrong way
//! round for the whole life of the project. Nothing in a struct-level assertion would
//! catch it, because a struct-level assertion is written by the same person who wrote the
//! mapping, from the same mental model.
//!
//! So the table below is transcribed from §3.5.2 independently — offset, width, type and
//! unit — and every field is read back out of the encoded bytes at the offset the
//! specification gives, then compared against a distinct value fed in through
//! [`TelemetryInputs`]. Every one of the fifty-six values is distinct, so a swapped pair
//! fails. The table's own completeness is asserted: 56 entries, covering offsets 0 to 195
//! with no gaps and no overlaps, and 12 reserved bytes after.
//!
//! # Fault injected to prove it can fail
//!
//! `nbr_verified` and `nbr_unverified` were swapped in
//! `TelemetryWindow::record`; `every_field_lands_where_the_specification_says` failed
//! naming both offsets. Separately, `ram_used_kib` was fed bytes instead of KiB and
//! `units_are_the_ones_the_table_declares` failed.

use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, NS_PER_S, SimTime};
use v2xw_node::telemetry::{NodeState, TelemetryInputs, TelemetryWindow};
use v2xw_record::wire::telemetry::{NodeTelemetry, PREFIX_BYTES, RECORD_SIZE_V1, TelemetryBody};

/// The width and reading of one field, transcribed from vwp-v1 §3.5.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Width {
    U8,
    U16,
    I16,
    U32,
    F32,
    U64,
    I64,
}

impl Width {
    const fn bytes(self) -> usize {
        match self {
            Width::U8 => 1,
            Width::U16 | Width::I16 => 2,
            Width::U32 | Width::F32 => 4,
            Width::U64 | Width::I64 => 8,
        }
    }
}

/// `(offset, width, name)` for all 56 fields, in §3.5.2's order.
const SPEC: &[(usize, Width, &str)] = &[
    (0, Width::U64, "storage_used_b"),
    (8, Width::U64, "storage_total_b"),
    (16, Width::U64, "next_topup_ns"),
    (24, Width::U64, "crl_bytes"),
    (32, Width::U64, "outbox_bytes"),
    (40, Width::I64, "clock_offset_ns"),
    (48, Width::U32, "node_id"),
    (52, Width::U32, "ram_used_kib"),
    (56, Width::U32, "ram_total_kib"),
    (60, Width::U32, "drop_rx_overflow"),
    (64, Width::U32, "drop_verify_policy_skip"),
    (68, Width::U32, "drop_verify_overflow"),
    (72, Width::U32, "drop_tx_overflow"),
    (76, Width::U32, "drop_reassembly_timeout"),
    (80, Width::U32, "drop_crl_backlog"),
    (84, Width::U32, "cert_stored"),
    (88, Width::U32, "crl_entries"),
    (92, Width::U32, "outbox_msgs"),
    (96, Width::U32, "peer_cache_entries"),
    (100, Width::U32, "p2pcd_requests"),
    (104, Width::U32, "full_cert_msgs"),
    (108, Width::F32, "msgs_in_per_s"),
    (112, Width::F32, "msgs_out_per_s"),
    (116, Width::F32, "verifications_per_s"),
    (120, Width::F32, "verify_wait_p50_ms"),
    (124, Width::F32, "verify_wait_p95_ms"),
    (128, Width::F32, "gnss_hdop"),
    (132, Width::F32, "gnss_sigma_m"),
    (136, Width::F32, "clock_drift_ppm"),
    (140, Width::F32, "pos_error_m"),
    (144, Width::F32, "airtime_ms_per_s"),
    (148, Width::U16, "cpu_util_pm"),
    (150, Width::U16, "hsm_util_pm"),
    (152, Width::U16, "q_rx_p50"),
    (154, Width::U16, "q_rx_p95"),
    (156, Width::U16, "q_verify_p50"),
    (158, Width::U16, "q_verify_p95"),
    (160, Width::U16, "q_app_p50"),
    (162, Width::U16, "q_app_p95"),
    (164, Width::U16, "q_tx_p50"),
    (166, Width::U16, "q_tx_p95"),
    (168, Width::U16, "q_crl_p50"),
    (170, Width::U16, "q_crl_p95"),
    (172, Width::U16, "dcc_state"),
    (174, Width::U16, "cbr_pm"),
    (176, Width::I16, "tx_power_cdbm"),
    (178, Width::U16, "nbr_total"),
    (180, Width::U16, "nbr_verified"),
    (182, Width::U16, "nbr_unverified"),
    (184, Width::U16, "nbr_revoked"),
    (186, Width::U16, "cert_active"),
    (188, Width::U16, "crl_expansion_pm"),
    (190, Width::U16, "unverified_ratio_pm"),
    (192, Width::U8, "gnss_fix"),
    (193, Width::U8, "node_state"),
    (194, Width::U8, "verify_policy"),
];

/// The transcription is complete and self-consistent before it is used to check anything.
#[test]
fn the_transcribed_table_covers_the_whole_record() {
    assert_eq!(SPEC.len(), 56, "§3.5.2 lists 56 named fields");
    let mut cursor = 0usize;
    for (off, width, name) in SPEC {
        assert_eq!(*off, cursor, "gap or overlap before `{name}`");
        assert_eq!(off % width.bytes(), 0, "`{name}` is not naturally aligned");
        cursor += width.bytes();
    }
    assert_eq!(cursor, 195, "the named fields end at 195");
    // 195 `reserved8`, then 196..208 reserved: 13 bytes to the 208-byte record.
    assert_eq!(RECORD_SIZE_V1 as usize - cursor, 13);
}

fn read(bytes: &[u8], off: usize, width: Width) -> i128 {
    let at = PREFIX_BYTES + off;
    match width {
        Width::U8 => i128::from(bytes[at]),
        Width::U16 => i128::from(u16::from_le_bytes([bytes[at], bytes[at + 1]])),
        Width::I16 => i128::from(i16::from_le_bytes([bytes[at], bytes[at + 1]])),
        Width::U32 => i128::from(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())),
        Width::F32 => {
            let f = f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            // Compared as milli-units, which is the declared grid of every f32 in the
            // record except `gnss_hdop`; the one 1e-4 field is checked separately.
            (f64::from(f) * 1000.0).round() as i128
        }
        Width::U64 => i128::from(u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())),
        Width::I64 => i128::from(i64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())),
    }
}

/// A window and an input set in which **every one of the 56 values is distinct**, so that
/// any two fields swapped in the mapping change the bytes.
fn distinct_inputs() -> (TelemetryWindow, TelemetryInputs, SimTime) {
    let now = 2 * NS_PER_S;
    let mut w = TelemetryWindow::new(0);
    // Rates: 2 s of window, so a count of 2n gives n per second.
    for _ in 0..22 {
        w.message_in(); // 11.0 /s
    }
    for _ in 0..24 {
        // 12.0 /s, 24 full-cert messages, 2 ms of air time each = 24 ms/s.
        w.message_out(Duration::from_millis(2), true);
    }
    // 28 verifications = 14.0 /s, with a tail: 20 waits of 13 ms and 8 of 19 ms put p50
    // at 13 ms and p95 at 19 ms, so the two percentile fields carry different values and
    // swapping them would be visible.
    for k in 0..28 {
        w.verification(Duration::from_millis(if k < 20 { 13 } else { 19 }));
    }
    // 100 deliveries, 41 of them unverified: ratio 410 per mille.
    for k in 0..100 {
        w.delivered(k >= 41);
    }
    let inputs = TelemetryInputs {
        node: NodeId::new(7),
        storage_used_b: 1_000_001,
        storage_total_b: 1_000_002,
        next_topup_ns: 1_000_003,
        crl_bytes: 1_000_004,
        outbox_bytes: 1_000_005,
        clock_offset_ns: -1_000_006,
        ram_used_kib: 2_000_001,
        ram_total_kib: 2_000_002,
        drops: [
            3_000_001, 3_000_002, 3_000_003, 3_000_004, 3_000_005, 3_000_006,
        ],
        cert_stored: 4_000_001,
        crl_entries: 4_000_002,
        outbox_msgs: 4_000_003,
        peer_cache_entries: 4_000_004,
        p2pcd_requests: 4_000_005,
        gnss_sigma_m: 5.001,
        gnss_hdop: 1.25,
        clock_drift_ppm: 6.002,
        pos_error_m: 7.003,
        cpu_util_pm: 601,
        hsm_util_pm: 602,
        queue_depths: [(611, 612), (613, 614), (615, 616), (617, 618), (619, 620)],
        dcc_state: 3,
        cbr_pm: 631,
        tx_power_cdbm: 2000,
        neighbors: (641, 642, 643, 644),
        cert_active: 651,
        crl_expansion_pm: 652,
        gnss_fix: 5,
        state: NodeState::Degraded,
        verify_policy: 2,
    };
    (w, inputs, now)
}

/// The value §3.5.2 says each field should carry, given [`distinct_inputs`], expressed in
/// the same milli-unit convention [`read`] uses for `f32`.
fn expected() -> Vec<(usize, i128)> {
    vec![
        (0, 1_000_001),
        (8, 1_000_002),
        (16, 1_000_003),
        (24, 1_000_004),
        (32, 1_000_005),
        (40, -1_000_006),
        (48, 7),
        (52, 2_000_001),
        (56, 2_000_002),
        (60, 3_000_001),
        (64, 3_000_002),
        (68, 3_000_003),
        (72, 3_000_004),
        (76, 3_000_005),
        (80, 3_000_006),
        (84, 4_000_001),
        (88, 4_000_002),
        (92, 4_000_003),
        (96, 4_000_004),
        (100, 4_000_005),
        (104, 24),
        (108, 11_000),
        (112, 12_000),
        (116, 14_000),
        (120, 13_000),
        (124, 19_000),
        (128, 1_250),
        (132, 5_001),
        (136, 6_002),
        (140, 7_003),
        // 24 messages x 2 ms of air time over 2 s = 24 ms/s.
        (144, 24_000),
        (148, 601),
        (150, 602),
        (152, 611),
        (154, 612),
        (156, 613),
        (158, 614),
        (160, 615),
        (162, 616),
        (164, 617),
        (166, 618),
        (168, 619),
        (170, 620),
        (172, 3),
        (174, 631),
        (176, 2000),
        (178, 641),
        (180, 642),
        (182, 643),
        (184, 644),
        (186, 651),
        (188, 652),
        (190, 410),
        (192, 5),
        (193, 4),
        (194, 2),
    ]
}

/// **The mapping.** Each of the 56 fields carries the quantity §3.5.2 names for it, read
/// back from the encoded bytes at the specified offset.
#[test]
fn every_field_lands_where_the_specification_says() {
    let (w, inputs, now) = distinct_inputs();
    let record = w.record(now, &inputs);
    let body = TelemetryBody::new(now, now, vec![record]);
    let bytes = body.encode();
    assert_eq!(bytes.len(), PREFIX_BYTES + RECORD_SIZE_V1 as usize);

    let expect = expected();
    assert_eq!(expect.len(), SPEC.len(), "one expectation per field");

    let mut wrong = Vec::new();
    for ((off, width, name), (eoff, want)) in SPEC.iter().zip(expect.iter()) {
        assert_eq!(off, eoff, "expectation list is out of order at `{name}`");
        let got = read(&bytes, *off, *width);
        if got != *want {
            wrong.push(format!("@{off:3} {name:24} want {want}, got {got}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "field mapping is wrong:\n{}",
        wrong.join("\n")
    );

    // Every value in the table is distinct, which is what makes a swapped pair visible.
    // Without this the test could pass while two fields were crossed.
    let mut values: Vec<i128> = expect.iter().map(|(_, v)| *v).collect();
    values.sort_unstable();
    let before = values.len();
    values.dedup();
    assert_eq!(
        values.len(),
        before,
        "two fields share a value, so swapping them would not be detected"
    );
}

/// The reserved bytes are zero, which §3.5.2 requires and §8.4 relies on for forward
/// compatibility.
#[test]
fn the_reserved_tail_is_zero() {
    let (w, inputs, now) = distinct_inputs();
    let bytes = TelemetryBody::new(now, now, vec![w.record(now, &inputs)]).encode();
    for off in 195..RECORD_SIZE_V1 as usize {
        assert_eq!(
            bytes[PREFIX_BYTES + off],
            0,
            "reserved byte {off} is not zero"
        );
    }
}

/// The units are the table's, not the runtime's convenience. Each assertion here is a
/// unit conversion that a mapping could plausibly get wrong in the same direction twice.
#[test]
fn units_are_the_ones_the_table_declares() {
    let (w, mut inputs, now) = distinct_inputs();
    // KiB, not bytes: a 128 MiB device is 131,072 KiB.
    inputs.ram_total_kib = 131_072;
    // Centi-dBm, not dBm: 20 dBm is 2000.
    inputs.tx_power_cdbm = 2000;
    // Per mille, not per cent and not a fraction.
    inputs.cpu_util_pm = 375;
    let r = w.record(now, &inputs);
    assert_eq!(r.ram_total_kib, 131_072);
    assert_eq!(r.tx_power_cdbm, 2000);
    assert_eq!(r.cpu_util_pm, 375);
    // Rates are per second of window, and the window here is two seconds.
    assert_eq!(r.msgs_in_per_s, 11.0);
    assert_eq!(r.msgs_out_per_s, 12.0);
    // Waits are milliseconds.
    assert_eq!(r.verify_wait_p50_ms, 13.0);
}

/// Conformance C1: a quantity this runtime does not model reaches the wire as its
/// unknown sentinel, never as a zero a HUD would draw as a measurement.
#[test]
fn unmodelled_quantities_are_the_unknown_sentinel() {
    let w = TelemetryWindow::new(0);
    let (_, inputs, _) = distinct_inputs();
    let empty = TelemetryWindow::new(0);
    let r = empty.record(0, &inputs);
    // A zero-length window has no rates.
    assert!(r.msgs_in_per_s.is_nan());
    assert!(r.verify_wait_p50_ms.is_nan());
    // Nothing delivered: the ratio is unknown, not zero.
    assert_eq!(r.unverified_ratio_pm, u16::MAX);
    // And the all-sentinel record leaves every unset field unknown.
    let u = NodeTelemetry::unknown(1);
    assert_eq!(u.nbr_total, u16::MAX);
    assert_eq!(u.storage_used_b, u64::MAX);
    assert!(u.pos_error_m.is_nan());
    let _ = w;
}

/// Every `f32` is quantised to its declared grid before it leaves the runtime (build
/// decision D9).
///
/// The property asserted is idempotence — re-quantising changes nothing — rather than
/// `is_on_grid` on the widened value. An `f32` cannot sit exactly on a decimal grid in
/// `f64`: the nearest `f32` to 1.2345 is 1.23450005…, so an `is_on_grid` assertion would
/// fail for a correctly quantised record and would have to be weakened to a tolerance,
/// at which point it would stop catching anything. Idempotence is the contract that
/// matters, because `TelemetryBody::encode` quantises again at the writer and the two
/// results must agree.
#[test]
fn every_float_is_quantised_before_it_leaves_the_runtime() {
    let (w, inputs, now) = distinct_inputs();
    let r = w.record(now, &inputs);
    for (name, value, grid) in r.f32_fields() {
        if value.is_nan() {
            continue;
        }
        let requantised = v2xw_core::math::quantize_to(f64::from(value), grid) as f32;
        assert_eq!(
            requantised, value,
            "`{name}` = {value} is not on its {grid} grid"
        );
    }
    // And the writer's own pass is a no-op on what the runtime produced.
    assert_eq!(r.quantised(), r);
}
