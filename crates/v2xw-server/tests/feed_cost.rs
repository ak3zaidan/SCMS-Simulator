//! What the followed vehicle's message feed costs the engine: a measurement, not a gate.
//!
//! Two runs of the same scenario and seed are stepped as fast as the kernel allows over the
//! same window of simulated time. One follows nobody; the other asks for the feed of one
//! vehicle every other step (5 pushes per simulated second, more than the page's 4 per wall
//! second at 1x). The difference in wall time is the cost of building and serialising the
//! pushes; the store that fills every node's log is on in both, and its size is printed.
//!
//! Ignored by default because it is a timing, and timings are not assertions on a shared
//! machine. Run it with:
//!
//! ```text
//! cargo test -p v2xw-server --test feed_cost -- --ignored --nocapture
//! ```
//!
//! `FEED_COST_RATE` sets the demand (veh/h), `FEED_COST_WARM_S` and `FEED_COST_SPAN_S` the
//! warm-up and the measured window.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use v2xw_server::Run;
use v2xw_server::feed::FeedLimits;
use v2xw_server::live::{LiveEngine, LiveOptions};
use v2xw_server::rpc::{self, Context};

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn scenario(rate: f64, duration: f64) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("duration_s: 60.0", &format!("duration_s: {duration:.1}"))
        .replace(
            "rate_veh_per_h: 30.0",
            &format!("rate_veh_per_h: {rate:.1}"),
        );
    let path = std::env::temp_dir().join("v2xw-feed-cost.yaml");
    std::fs::write(&path, text).expect("write scenario");
    path
}

fn run_to(run: &Run, t_ns: u64) {
    while run.sim_time() < t_ns {
        if !run.tick().expect("the engine runs") {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

struct Measured {
    wall_s: f64,
    steps: u64,
    actors: usize,
    pushes: usize,
    push_ms: Vec<f64>,
    push_bytes: Vec<usize>,
    store: String,
}

fn measure(path: &Path, warm_ns: u64, span_ns: u64, with_feed: bool) -> Measured {
    let engine = LiveEngine::open(
        path,
        LiveOptions {
            build_utc: "2026-09-24T00:00:00Z".to_string(),
            paused: true,
            speed: 0.0,
            ..LiveOptions::default()
        },
    )
    .expect("build");
    let world_json = engine.world_json().to_string();
    let run = Run::new(Box::new(engine), world_json).expect("run");
    for (method, params) in [
        ("run.start", json!({"paused": true, "speed": 0})),
        ("run.resume", json!({})),
    ] {
        let text =
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        let request = rpc::parse(&text).expect("parse");
        let mut ctx = Context {
            run: &run,
            session: None,
            pending: None,
            received_at: None,
        };
        rpc::dispatch(&mut ctx, &request).expect(method);
    }
    run_to(&run, warm_ns);
    let actors = run.counts().0 as usize;
    let node = run
        .nodes()
        .iter()
        .map(|r| r.node_id)
        .next()
        .expect("a node to follow");
    let limits = FeedLimits {
        sent: 25,
        received: 50,
        waiting: 8,
        bytes: true,
    };
    let mut after = None;
    let (mut push_ms, mut push_bytes) = (Vec::new(), Vec::new());
    let start = Instant::now();
    let first = run.sim_time();
    let mut steps = 0u64;
    while run.sim_time() < warm_ns + span_ns {
        if !run.tick().expect("the engine runs") {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        steps += 1;
        if with_feed && steps % 2 == 0 {
            let t = Instant::now();
            let feed = run
                .node_feed(node, after, &limits, true)
                .expect("a live feed");
            let text = feed.to_string();
            push_ms.push(t.elapsed().as_secs_f64() * 1e3);
            push_bytes.push(text.len());
            after = feed["t_ns"].as_u64();
        }
    }
    let wall_s = start.elapsed().as_secs_f64();
    let diag = run.diagnostics();
    let _ = first;
    Measured {
        wall_s,
        steps,
        actors,
        pushes: push_ms.len(),
        push_ms,
        push_bytes,
        store: diag["feed_store"].to_string(),
    }
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * q).ceil() as usize)
        .saturating_sub(1)
        .min(v.len() - 1)]
}

#[test]
#[ignore = "a timing; run with --ignored --nocapture"]
fn the_feed_costs_this_much() {
    let rate = env_f64("FEED_COST_RATE", 60_000.0);
    let warm_s = env_f64("FEED_COST_WARM_S", 30.0);
    let span_s = env_f64("FEED_COST_SPAN_S", 10.0);
    let path = scenario(rate, warm_s + span_s + 5.0);
    let (warm, span) = ((warm_s * 1e9) as u64, (span_s * 1e9) as u64);
    let off = measure(&path, warm, span, false);
    let mut on = measure(&path, warm, span, true);
    let rate_off = off.steps as f64 / off.wall_s;
    let rate_on = on.steps as f64 / on.wall_s;
    let bytes: Vec<f64> = on.push_bytes.iter().map(|b| *b as f64).collect();
    let mean_bytes = bytes.iter().sum::<f64>() / bytes.len().max(1) as f64;
    println!(
        "demand {rate} veh/h, {} actors at {warm_s} s; window {span_s} s of simulated time",
        off.actors
    );
    println!(
        "feed off: {} steps in {:.2} s = {rate_off:.1} steps/s; store {}",
        off.steps, off.wall_s, off.store
    );
    println!(
        "feed on:  {} steps in {:.2} s = {rate_on:.1} steps/s ({:+.1} %); {} pushes, build+serialise p50 {:.2} ms p95 {:.2} ms, mean {:.0} B",
        on.steps,
        on.wall_s,
        (rate_on / rate_off - 1.0) * 100.0,
        on.pushes,
        pct(&mut on.push_ms, 0.5),
        pct(&mut on.push_ms, 0.95),
        mean_bytes,
    );
}
