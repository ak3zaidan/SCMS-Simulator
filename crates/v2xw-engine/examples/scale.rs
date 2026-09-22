//! `scale` — what one scenario costs, and what its reception path actually produced.
//!
//! The run report and the MCAP recording answer different questions from this one. The CLI
//! writes a recording, and at ten thousand nodes the recording *is* the cost — a `phy.rx`
//! record per reception attempt is tens of gigabytes of JSON — so a wall-clock number
//! taken with a recorder attached measures the recorder. This example runs the same engine
//! with three recorders and prints all three, which is the only way to say which half of
//! the time is the simulation and which is the writing down.
//!
//! ```text
//! cargo run --release -p v2xw-engine --example scale -- <scenario.yaml> [--curve] [--repeat N]
//! ```
//!
//! * `--curve` additionally runs with a [`MemoryRecorder`] and prints packet delivery
//!   ratio against transmitter-to-receiver distance in 25 m bins — the bin width
//!   `v2xw_radio::abstract_tier::DISTANCE_BIN_M` calibrates on, so the curve is directly
//!   comparable with the abstract tier's table and with the published measurements
//!   04-models.md §13 lists.
//! * `--repeat N` runs the whole thing N times and compares the content digests, which is
//!   the determinism check at a size where holding the records is not possible.
//!
//! It is an example rather than a test because it is a measurement: it has no pass
//! condition, and the numbers it prints are the answer.

use std::collections::BTreeMap;
use std::time::Instant;

use v2xw_engine::{DigestRecorder, Engine, MemoryRecorder, NullRecorder, RunReport, Scenario};

/// The distance bin width of the curve, metres.
const BIN_M: f64 = v2xw_radio::abstract_tier::DISTANCE_BIN_M;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or("usage: scale <scenario.yaml> [--curve] [--repeat N]")?;
    let curve = args.iter().any(|a| a == "--curve");
    // The digest pass runs the whole scenario a second time. At the sizes where that
    // matters it is also the pass whose cost is least interesting, so it is skippable.
    let quick = args.iter().any(|a| a == "--quick");
    // Six agents compile on this machine at once, so one timing is a timing of the load.
    // The null pass is run `passes` times and the **best** is reported: the minimum is the
    // run least disturbed by everything else, and it is the only statistic of a wall-clock
    // measurement on a shared machine that means anything.
    let passes: usize = args
        .iter()
        .position(|a| a == "--passes")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let repeat: usize = args
        .iter()
        .position(|a| a == "--repeat")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);

    let scenario = Scenario::load(path)?;
    let duration_s = scenario.time.duration_s;
    println!("scenario        {}", scenario.meta.name);
    println!("seed            0x{:x}", scenario.seed);
    println!("duration        {duration_s} s");

    // The build is not the run: the world import dominates it and it is paid once per
    // process, so it is timed and reported separately rather than folded in.
    let t = Instant::now();
    let mut engine = Engine::build(scenario.clone(), "")?;
    let build_s = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let mut null = NullRecorder::new();
    let report = engine.run(&mut null)?;
    let mut run_null_s = t.elapsed().as_secs_f64();
    let mut worst = run_null_s;
    for _ in 1..passes {
        let mut engine = Engine::build(scenario.clone(), "")?;
        let mut null = NullRecorder::new();
        let t = Instant::now();
        let again = engine.run(&mut null)?;
        let elapsed = t.elapsed().as_secs_f64();
        assert_eq!(again, report, "two runs of one scenario disagreed");
        run_null_s = run_null_s.min(elapsed);
        worst = worst.max(elapsed);
    }

    let mut digest = DigestRecorder::new();
    let mut run_digest_s = f64::NAN;
    if !quick {
        let mut engine = Engine::build(scenario.clone(), "")?;
        let t = Instant::now();
        let report_digest = engine.run(&mut digest)?;
        run_digest_s = t.elapsed().as_secs_f64();
        assert_eq!(
            report, report_digest,
            "the recorder changed the run, which it may not"
        );
    }

    println!("\n-- cost ---------------------------------------------------");
    println!("build           {build_s:.3} s (world import, once per process)");
    println!(
        "run, null       {run_null_s:.3} s  = {:.4} wall s per simulated s  (best of {passes}, worst {worst:.3} s)",
        run_null_s / duration_s
    );
    if quick {
        println!("run, digest     skipped (--quick)");
    } else {
        println!(
            "run, digest     {run_digest_s:.3} s  = {:.4} wall s per simulated s  (+{:.1} % for hashing every record)",
            run_digest_s / duration_s,
            100.0 * (run_digest_s - run_null_s) / run_null_s.max(1e-9)
        );
    }

    print_report(&report);

    println!("\n-- what a recorder is handed ------------------------------");
    let mut total_bytes = 0u64;
    for (channel, (count, bytes)) in digest.per_channel() {
        total_bytes += bytes;
        println!("  {channel:22} {count:>12} records {bytes:>14} json bytes");
    }
    println!("  {:22} {:>12} records {total_bytes:>14} json bytes", "TOTAL", digest.written());
    println!("content digest  {}", digest.digest_hex());

    for i in 1..repeat {
        let mut engine = Engine::build(scenario.clone(), "")?;
        let mut again = DigestRecorder::new();
        let report_again = engine.run(&mut again)?;
        let same_digest = again.digest_hex() == digest.digest_hex();
        println!(
            "repeat {i:<3}      digest {} | report {}",
            if same_digest { "IDENTICAL" } else { "DIFFERENT" },
            if report_again == report {
                "IDENTICAL"
            } else {
                "DIFFERENT"
            }
        );
    }

    if curve {
        let mut engine = Engine::build(scenario, "")?;
        let mut memory = MemoryRecorder::new();
        engine.run(&mut memory)?;
        print_curve(&memory);
    }
    Ok(())
}

fn print_report(report: &RunReport) {
    println!("\n-- the run ------------------------------------------------");
    println!("nodes created   {}", report.nodes_created);
    println!("frames sent     {}", report.frames_transmitted);
    println!(
        "mac             {} granted, {} dropped, mean access delay {:.0} µs",
        report.mac_grants,
        report.mac_drops,
        if report.mac_grants > 0 {
            report.mac_access_delay_ns as f64 / report.mac_grants as f64 / 1000.0
        } else {
            0.0
        }
    );
    println!(
        "receptions      {} attempted, {} decoded, {} frames reached >= 1 node",
        report.reception_attempts, report.receptions_ok, report.frames_received
    );
    match report.pdr() {
        Some(pdr) => println!("pdr             {pdr:.4} over reception attempts"),
        None => println!("pdr             n/a: nothing was attempted"),
    }
    let p2 = &report.phase2;
    if p2 != &v2xw_engine::phase2::Phase2Report::default() {
        println!("\n-- the phase 2 path ---------------------------------------");
        println!("rsus            {}", p2.rsus);
        println!(
            "attackers       {} armed, {} claims falsified",
            p2.attackers, p2.falsified_claims
        );
        println!(
            "detection       {} messages checked, {} verdicts fired",
            p2.messages_checked, p2.verdicts_fired
        );
        println!(
            "reports         {} on the air, {} reached the authority",
            p2.reports_sent, p2.reports_received
        );
        println!(
            "ma cases        {} opened, {} refused by the linkage authorities",
            p2.cases_opened, p2.cases_unresolved
        );
        println!(
            "revocation      {} issued, {} broadcast, {} installed, {} receptions revoked",
            p2.crls_issued, p2.crl_broadcasts, p2.crls_installed, p2.revoked_receptions
        );
        println!(
            "backend latency {:.3} s from the detector firing to the device enforcing",
            p2.revocation_latency_ns as f64 / 1e9
        );
    }
    let lost: u64 = report.rx_losses.values().sum();
    for (cause, n) in &report.rx_losses {
        println!(
            "  lost {cause:<20} {n:>12}  ({:.1} % of losses)",
            100.0 * *n as f64 / lost.max(1) as f64
        );
    }
}

/// Packet delivery ratio against distance, from the recorded `phy.rx` stream.
///
/// Read off the records rather than accumulated inside the engine, for the reason
/// 03-interfaces.md §10 gives for metric providers: what is measured has to be what was
/// written down, or a replay and a live run can disagree.
fn print_curve(memory: &MemoryRecorder) {
    let mut bins: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    let mut causes: BTreeMap<u64, BTreeMap<String, u64>> = BTreeMap::new();
    for (_, record) in memory.records() {
        if record.channel != "phy.rx" {
            continue;
        }
        let Ok(view) = v2xw_metrics::channels::decode::<v2xw_metrics::channels::PhyRxView>(record)
        else {
            continue;
        };
        let Some(d) = view.dist_m else { continue };
        let bin = (d / BIN_M) as u64;
        let entry = bins.entry(bin).or_insert((0, 0));
        entry.0 += 1;
        if matches!(view.outcome, v2xw_metrics::channels::RxOutcome::Ok) {
            entry.1 += 1;
        }
        for cause in view.all_causes() {
            *causes
                .entry(bin)
                .or_default()
                .entry(cause.to_string())
                .or_insert(0) += 1;
        }
    }
    println!("\n-- packet delivery ratio vs distance ----------------------");
    println!("  {:>12}  {:>10}  {:>10}  {:>7}  dominant loss", "distance m", "attempts", "decoded", "pdr");
    for (bin, (attempts, ok)) in &bins {
        let lo = *bin as f64 * BIN_M;
        let dominant = causes
            .get(bin)
            .and_then(|c| c.iter().max_by_key(|(_, n)| **n))
            .map(|(cause, n)| format!("{cause} ({n})"))
            .unwrap_or_default();
        println!(
            "  {:>5.0}-{:<6.0}  {attempts:>10}  {ok:>10}  {:>7.4}  {dominant}",
            lo,
            lo + BIN_M,
            *ok as f64 / *attempts as f64
        );
    }
}
