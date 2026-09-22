//! The acceptance test: the real TypeScript client against this server.
//!
//! `ui/packages/protocol` was written from `docs/protocol/vwp-v1.md` independently of any
//! server, so a suite that passes against it is evidence about the specification rather
//! than about a shared implementation. `tests/ts/conformance.mjs` holds the assertions —
//! ported from the mock server's own interop suite, minus the ones that describe the
//! mock's synthetic Manhattan geometry — and this test is what makes them run in CI.
//!
//! Four servers are started, because the checks need runs of particular shapes: a long
//! fixture run, a three-second run for the end-of-run path (§2.3, §3.11), a replay of a
//! recording (§7), and — the one that matters — **a real `v2xw-engine` run**.
//!
//! The fixture sections and the kernel section assert different things on purpose. The
//! fixture's shape is known (seven actor classes, RSUs in the fleet, a ground-truth metric
//! in the catalogue), so its sections can assert it; none of that is a property of the
//! protocol, and a kernel run has none of it. The kernel's section therefore asserts only
//! what `docs/protocol/vwp-v1.md` says, which is what makes it evidence about a live run
//! rather than about a fixture.
//!
//! # Skipping, and why it is announced loudly
//!
//! The harness needs `node` and the client's built `dist`. When either is missing the
//! test prints what is missing and passes, because a Rust-only checkout cannot be asked
//! to build a TypeScript package. It does **not** skip when both are present and the
//! harness fails: that is a failure. The distinction matters — a check that quietly
//! disappears is worse than no check, so the skip names itself in the test output.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use v2xw_record::fixture::{self, RunShape};
use v2xw_server::{
    LiveOptions, ServerOptions, StubOptions, serve_replay, serve_scenario, serve_stub,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

fn ephemeral() -> ServerOptions {
    ServerOptions {
        bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        ..ServerOptions::default()
    }
}

fn world() -> (Arc<v2xw_world::WorldPayload>, String) {
    let params = v2xw_world::procedural::GridParams {
        cols: 4,
        rows: 4,
        signalised: true,
        block_buildings: true,
        rsu_at_junctions: true,
        ..v2xw_world::procedural::GridParams::legacy()
    };
    let world =
        v2xw_world::procedural::grid(&params, &v2xw_world::ImportOptions::default()).expect("grid");
    let payload = v2xw_world::serde_vwp::write(&world).expect("payload");
    let json = v2xw_world::serde_vwp::to_json_string(&world).expect("json");
    (Arc::new(payload), json)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_real_typescript_client_conforms_against_this_server() {
    let root = repo_root();
    let harness = root.join("crates/v2xw-server/tests/ts/conformance.mjs");
    let client_dist = root.join("ui/packages/protocol/dist/index.js");
    if !client_dist.exists() {
        println!(
            "SKIPPED: {} is not built. Run `pnpm -C ui --filter @vwp/protocol build` to \
             enable the acceptance test.",
            client_dist.display()
        );
        return;
    }
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        println!("SKIPPED: `node` is not on PATH, so the TypeScript client cannot be run.");
        return;
    }

    // 1. the live run the bulk of the suite drives.
    let live = serve_stub(
        ephemeral(),
        StubOptions {
            actors: 60,
            grid: 6,
            duration_s: 600,
            ..StubOptions::default()
        },
    )
    .await
    .expect("live server");

    // 2. a three-second run, so the end-of-run path actually runs.
    let short = serve_stub(
        ephemeral(),
        StubOptions {
            actors: 8,
            grid: 3,
            duration_s: 3,
            // Paused, because the suite's earlier sections take tens of seconds and a
            // three-second run left advancing would be over before anything looked at it.
            // The harness resumes it when it gets there.
            paused: true,
            ..StubOptions::default()
        },
    )
    .await
    .expect("short server");

    // 3. a real kernel run. The demand rate is raised off `phase1-grid.yaml`'s 30 veh/h,
    // which realises one vehicle under this seed: the assertions are about a fleet — a
    // node table, events between nodes, a metric with trials behind it — and one vehicle
    // would make them vacuous rather than false.
    let scenario_dir = fixture::scratch_dir("interop-live").expect("scratch dir");
    let scenario_path = scenario_dir.join("interop-live.yaml");
    let scenario_text = std::fs::read_to_string(root.join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 4000.0")
        .replace("duration_s: 60.0", "duration_s: 600.0");
    std::fs::write(&scenario_path, scenario_text).expect("write scenario");
    let live_engine = serve_scenario(
        ephemeral(),
        &scenario_path,
        LiveOptions {
            // The manifest timestamp is the caller's: no part of the engine may read a
            // clock, and a fixed one keeps two runs of this test comparable.
            build_utc: "2026-09-22T00:00:00Z".to_string(),
            ..LiveOptions::default()
        },
    )
    .await
    .expect("live engine server");

    // 4. a replay of a recording, for §7.
    let dir = fixture::scratch_dir("interop-replay").expect("scratch dir");
    let path = dir.join("interop.mcap");
    fixture::write_recording(&path, &RunShape::new(6, 120)).expect("recording");
    let (payload, json) = world();
    let replay = serve_replay(ephemeral(), &path, payload, json)
        .await
        .expect("replay server");
    // A replay starts paused, and stays paused until the harness reaches §7: an
    // unthrottled replay of a 120-step recording finishes in milliseconds.

    let output = tokio::process::Command::new("node")
        .arg(&harness)
        .arg(live.http_url())
        .arg(short.http_url())
        .arg(replay.http_url())
        .arg(live_engine.http_url())
        .current_dir(&root)
        .output()
        .await
        .expect("run the harness");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}");
    if !stderr.trim().is_empty() {
        println!("stderr:\n{stderr}");
    }

    live.stop().await;
    short.stop().await;
    replay.stop().await;
    live_engine.stop().await;

    assert!(
        output.status.success(),
        "the TypeScript client's conformance suite failed against this server"
    );
    // The harness prints its own tally; a run that asserted nothing must not pass.
    assert!(
        stdout.contains(" passed, 0 failed"),
        "the harness did not report a clean tally"
    );
    let count: usize = stdout
        .rsplit_once('\n')
        .map(|_| ())
        .and_then(|()| {
            stdout
                .lines()
                .rev()
                .find_map(|line| line.trim().split_once(" passed,"))
                .and_then(|(n, _)| n.trim().parse().ok())
        })
        .unwrap_or(0);
    assert!(
        count >= 35,
        "the harness reported only {count} passing checks; it should run at least 35"
    );
}
