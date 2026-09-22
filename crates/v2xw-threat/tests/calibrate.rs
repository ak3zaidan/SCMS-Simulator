//! A calibration probe, not an assertion: prints what one harness configuration actually
//! produces so that the fleet size, the reception count and the node's delivery budget are
//! chosen from measurements instead of from a guess.
//!
//! Run it with `cargo test --release -p v2xw-threat --test calibrate -- --nocapture`.

mod common;

use common::sim::{self, SimOptions};
use v2xw_threat::attack::AttackKind;

#[test]
fn probe_the_fleet_and_the_channel() {
    for (label, rate, cap, step_ms) in [
        ("1hz", 14_400.0, 60u64, 1000u64),
        ("10hz", 14_400.0, 60, 100),
    ] {
        for ideal in [false, true] {
            let mut opts = SimOptions::new(11, rate, 0.25).with_attack(AttackKind::ConstPosOffset);
            opts.max_vehicles = cap;
            opts.step_ms = step_ms;
            if ideal {
                opts = opts.ideal();
            }
            let t = std::time::Instant::now();
            let out = sim::run(&opts);
            let (report, vehicle) = sim::score(&out);
            eprintln!("=== {label} ideal={ideal} ===\n{}", sim::summarize(&out));
            eprintln!(
                "{label:7} ideal={ideal:5} {:5.1}s | nodes {:3} att {:3} | frames {:6} \
                 falsified {:6} | rx {:8}/{:9} | delivered {:8} | reports {:6} rev {:3} | \
                 report tp/fp/fn/tn {}/{}/{}/{} | vehicle tp {} fp {}",
                t.elapsed().as_secs_f64(),
                out.nodes,
                out.attackers,
                out.frames,
                out.falsified_frames,
                out.receptions,
                out.reception_attempts,
                out.delivered,
                out.reports,
                out.revocations,
                report.tp,
                report.fp,
                report.fn_,
                report.tn,
                vehicle.tp,
                vehicle.fp,
            );
        }
    }
}
