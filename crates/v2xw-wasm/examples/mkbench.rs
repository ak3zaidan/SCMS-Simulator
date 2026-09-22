//! Writes the recording the WebAssembly seek benchmark measures against.
//!
//!   cargo run -p v2xw-wasm --release --example mkbench -- target/wasm-bench
//!
//! The shape is the one vwp-v1 §7.4's budget is written against and the one
//! `v2xw-record`'s `benches/seek.rs` uses — 400 actors over 600 s of simulated time, 4 MiB
//! chunk target — so the WebAssembly figure and the native 1.763 ms p95 are measurements
//! of the same work on the same file and can be put beside each other.
//!
//! It also runs the native seek loop over the identical targets and prints the native
//! numbers, so the comparison is made on this machine rather than against a figure from
//! another one. Timing is the one thing a benchmark may read a clock for; nothing in the
//! `v2xw-wasm` library does.

use std::time::Instant;

use v2xw_record::encoder::Cadence;
use v2xw_record::fixture::RunShape;
use v2xw_wasm::ReplaySession;

/// 400 actors and 600 s: §7.4's "busy downtown".
const ACTORS: u32 = 400;
/// Seconds of simulated time.
const SECONDS: u32 = 600;
/// Warm seeks, as the native benchmark runs.
const WARM_SEEKS: usize = 200;

/// The same deterministic generator the native benchmark uses, so both measure the same
/// 200 targets on the same file rather than two random samples.
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

/// Hyndman & Fan type 7, the interpolating quantile this project reports.
fn quantile_type7(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let h = q * (sorted.len() as f64 - 1.0);
    let lo = h.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let frac = h - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// Nearest-rank, which is what the §7.4 pass criterion means over 200 samples.
fn quantile_nearest(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (q * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/wasm-bench".to_string());
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("bench.mcap");

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
    v2xw_record::fixture::write_recording_with(
        &path,
        &shape,
        v2xw_record::RecordingOptions {
            cadence,
            // §7.1: chunks target 4 MiB uncompressed.
            chunk_target_bytes: 4 * 1024 * 1024,
            ..Default::default()
        },
        false,
    )?;
    let bytes = std::fs::read(&path)?;

    let mut session = ReplaySession::from_bytes(bytes.clone())?;
    let (min, max) = session.span()?.ok_or("no snapshot span")?;
    let index_chunks = session.index()?.chunks.len();
    let index_keyframes = session.index()?.keyframes.len();

    // The target list, written out so the JavaScript benchmark measures the same seeks.
    let mut rng = Lcg(0x9E37_79B9_7F4A_7C15);
    let mut targets = Vec::with_capacity(WARM_SEEKS);
    for _ in 0..WARM_SEEKS {
        targets.push(rng.next_in(min, max));
    }

    // Warm the page cache and the index the same way the JavaScript run will.
    for t in targets.iter().take(20) {
        session.seek(*t)?;
    }

    let mut samples = Vec::with_capacity(WARM_SEEKS);
    for t in &targets {
        let at = Instant::now();
        let report = session
            .seek(*t)?
            .done()
            .ok_or("a resident recording never waits")?;
        samples.push(at.elapsed().as_secs_f64() * 1e3);
        std::hint::black_box(report.actor_count);
    }
    samples.sort_by(f64::total_cmp);

    let mut manifest = String::new();
    manifest.push_str("{\n  \"file\": \"bench.mcap\",\n");
    manifest.push_str(&format!("  \"bytes\": {},\n", bytes.len()));
    manifest.push_str(&format!("  \"chunks\": {index_chunks},\n"));
    manifest.push_str(&format!("  \"keyframes\": {index_keyframes},\n"));
    manifest.push_str(&format!("  \"span_start_ns\": {min},\n"));
    manifest.push_str(&format!("  \"span_end_ns\": {max},\n"));
    manifest.push_str(&format!(
        "  \"native_p95_nearest_ms\": {:.4},\n",
        quantile_nearest(&samples, 0.95)
    ));
    manifest.push_str(&format!(
        "  \"native_p95_type7_ms\": {:.4},\n",
        quantile_type7(&samples, 0.95)
    ));
    manifest.push_str(&format!(
        "  \"native_max_ms\": {:.4},\n",
        samples.last().copied().unwrap_or(0.0)
    ));
    manifest.push_str("  \"targets_ns\": [");
    for (i, t) in targets.iter().enumerate() {
        if i > 0 {
            manifest.push(',');
        }
        manifest.push_str(&t.to_string());
    }
    manifest.push_str("]\n}\n");
    std::fs::write(dir.join("bench.json"), &manifest)?;

    println!(
        "bench.mcap: {} MiB, {index_chunks} chunks, {index_keyframes} keyframes",
        bytes.len() / (1024 * 1024)
    );
    println!(
        "native seek over {WARM_SEEKS} warm targets: p95 {:.3} ms (nearest-rank), {:.3} ms (type 7), max {:.3} ms",
        quantile_nearest(&samples, 0.95),
        quantile_type7(&samples, 0.95),
        samples.last().copied().unwrap_or(0.0)
    );
    Ok(())
}
