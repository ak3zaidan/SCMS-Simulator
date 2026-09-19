//! `bench_seek` — the ≤ 100 ms seek budget of §7.4 and conformance P2.
//!
//! §7.4 sets the pass criteria: "100 uniformly random seek targets, assert **p95 ≤ 100
//! ms** and **max ≤ 200 ms** … with the summary already cached. A second variant measures
//! the cold case (first seek) and asserts ≤ 150 ms."
//!
//! Run with `cargo bench -p v2xw-record`. The bench profile inherits release, which is
//! the only honest way to measure this: a debug build measures the optimiser's absence.
//!
//! # The one wall clock in the crate
//!
//! Everything else in `v2xw-record` takes its time from [`v2xw_core::SimTime`], because a
//! wall-clock read in engine-facing code destroys reproducibility. A latency benchmark is
//! the exception that proves the rule: it exists to measure wall time, and it is never
//! linked into the engine.

use std::time::{Duration, Instant};

use v2xw_record::encoder::Cadence;
use v2xw_record::fixture::{RunShape, scratch_dir, write_recording_with};
use v2xw_record::{Reader, RecordingOptions};

/// The shape §7.4's budget is written against: a 600 s recording of a busy downtown.
const ACTORS: u32 = 400;
const SECONDS: u32 = 600;
const WARM_SEEKS: usize = 200;
const COLD_SEEKS: usize = 20;

/// A deterministic generator, so the targets are the same on every machine.
struct Lcg(u64);

impl Lcg {
    fn next_in(&mut self, lo: u64, hi: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        if hi <= lo {
            return lo;
        }
        lo + (self.0 >> 11) % (hi - lo + 1)
    }
}

fn quantile(sorted: &[Duration], q: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    // Nearest-rank, which is what a pass criterion over 100 samples means.
    let rank = (q * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Hyndman & Fan type 7, the interpolating quantile the rest of the project reports
/// (`v2xw-metrics`' `DistributionSummary`) and the one an independent verifier measured
/// this bench against. Printed beside the nearest-rank figure so the two numbers are
/// comparable rather than a ranking artefact; the pass criterion stays on nearest-rank,
/// which is never the smaller of the two.
fn quantile_type7(sorted: &[Duration], q: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let h = q * (sorted.len() as f64 - 1.0);
    let lo = h.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let frac = h - lo as f64;
    let a = sorted[lo].as_secs_f64();
    let b = sorted[hi].as_secs_f64();
    Duration::from_secs_f64(a + (b - a) * frac)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() {
    let cadence = Cadence::DEFAULT;
    let steps = SECONDS * 1_000 / (cadence.mobility_step.as_nanos() / 1_000_000) as u32;
    let shape = RunShape {
        actors: ACTORS,
        steps,
        signals: 8,
        cadence,
        teleport_at: Some(steps / 3),
        ..Default::default()
    };

    let dir = scratch_dir("bench-seek").expect("a scratch directory");
    let path = dir.join("downtown.mcap");
    let build = Instant::now();
    let (frames, summary) = write_recording_with(
        &path,
        &shape,
        RecordingOptions {
            cadence,
            // §7.1: chunks target 4 MiB uncompressed.
            chunk_target_bytes: 4 * 1024 * 1024,
            ..Default::default()
        },
        false,
    )
    .expect("the recording is written");
    let build_ms = ms(build.elapsed());
    let bytes = std::fs::metadata(&path)
        .expect("the recording exists")
        .len();

    let mut reader = Reader::open_paged(&path).expect("the recording opens");
    let index = reader.index();
    let chunks = index.chunks.len();
    let keyframes = index.keyframes.len();
    let deltas = index.deltas.len();
    let (min, max) = index.snapshot_span().expect("a recorded span");

    println!(
        "recording: {} frames, {} MiB on disk, {chunks} chunks, {keyframes} keyframes, {deltas} deltas",
        frames.len(),
        bytes / (1024 * 1024)
    );
    println!(
        "           {ACTORS} actors, {SECONDS} s of simulated time, written in {build_ms:.0} ms ({} messages)",
        summary.message_count
    );

    // --- warm: the summary and the message indexes are already in memory (§7.4) -------
    let mut rng = Lcg(0x5EED_0000_0BAD_F00D);
    let mut warm = Vec::with_capacity(WARM_SEEKS);
    let mut deltas_returned = 0usize;
    let mut chunks_read = 0usize;
    for _ in 0..WARM_SEEKS {
        let t = rng.next_in(min, max);
        let start = Instant::now();
        let result = reader.seek(t).expect("the seek succeeds");
        warm.push(start.elapsed());
        deltas_returned += result.deltas.len();
        chunks_read += result.chunks_read;
        assert!(result.keyframe_time <= t);
    }
    warm.sort_unstable();

    // --- cold: a fresh reader per seek, so the footer, the summary and every message
    //     index are read from the file again (§7.4's second variant) -------------------
    let mut rng = Lcg(0xC01D_0000_0BAD_F00D);
    let mut cold = Vec::with_capacity(COLD_SEEKS);
    for _ in 0..COLD_SEEKS {
        let t = rng.next_in(min, max);
        let start = Instant::now();
        let mut fresh = Reader::open_paged(&path).expect("the recording opens");
        let result = fresh.seek(t).expect("the seek succeeds");
        cold.push(start.elapsed());
        assert!(result.keyframe_time <= t);
    }
    cold.sort_unstable();

    println!(
        "warm seek over {WARM_SEEKS} targets: p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, \
         max {:.3} ms  (mean {:.1} deltas, {:.2} chunks read)",
        ms(quantile(&warm, 0.50)),
        ms(quantile(&warm, 0.95)),
        ms(quantile(&warm, 0.99)),
        ms(*warm.last().expect("samples")),
        deltas_returned as f64 / WARM_SEEKS as f64,
        chunks_read as f64 / WARM_SEEKS as f64,
    );
    println!(
        "           the same samples on type-7 interpolation: p50 {:.3} ms, p95 {:.3} ms, \
         p99 {:.3} ms",
        ms(quantile_type7(&warm, 0.50)),
        ms(quantile_type7(&warm, 0.95)),
        ms(quantile_type7(&warm, 0.99)),
    );
    println!(
        "cold seek over {COLD_SEEKS} targets (reader opened each time): p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        ms(quantile(&cold, 0.50)),
        ms(quantile(&cold, 0.95)),
        ms(*cold.last().expect("samples")),
    );

    // §7.4's pass criteria, and P2's.
    let p95 = quantile(&warm, 0.95);
    let worst = *warm.last().expect("samples");
    let cold_worst = *cold.last().expect("samples");
    let mut failures = Vec::new();
    if p95 > Duration::from_millis(100) {
        failures.push(format!("warm p95 {:.3} ms exceeds 100 ms", ms(p95)));
    }
    if worst > Duration::from_millis(200) {
        failures.push(format!("warm max {:.3} ms exceeds 200 ms", ms(worst)));
    }
    if cold_worst > Duration::from_millis(150) {
        failures.push(format!("cold max {:.3} ms exceeds 150 ms", ms(cold_worst)));
    }
    if failures.is_empty() {
        println!("bench_seek: PASS against the §7.4 budget");
    } else {
        for f in &failures {
            println!("bench_seek: FAIL — {f}");
        }
        panic!("bench_seek missed the §7.4 budget: {}", failures.join("; "));
    }
}
