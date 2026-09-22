//! `v2xw-server` — serve one run over VWP v1.
//!
//! Two modes. `--scenario <path>` runs the real kernel: the scenario is loaded, the world
//! is imported and `v2xw-engine` produces the stream. With no `--scenario`, the synthetic
//! fixture engine runs instead, which is what a client developer points at when they want
//! a conforming stream and no city extract.

use std::net::{IpAddr, SocketAddr};

use v2xw_server::{LiveOptions, ServerOptions, StubOptions, serve_scenario, serve_stub};

const USAGE: &str = "\
v2xw-server — the VWP v1 control surface and stream (docs/protocol/vwp-v1.md)

  v2xw-server --scenario scenarios/phase1-manhattan.yaml
  v2xw-server [fixture options]

  live run (the real engine):
  --scenario <p>  scenario to run; without it the synthetic fixture runs instead
  --record <p>    write the MCAP recording here
  --build-utc <t> manifest build timestamp (the engine may not read a clock)
  --retain <n>    mobility steps of history kept for run.seek (default 36000)
  --lookahead <n> steps the kernel may run ahead of the stream (default 64)

  fixture run:
  --actors <n>    actors to drive (default 120)
  --grid <n>      junctions per side of the generated grid (default 8)
  --block <m>     junction spacing, metres (default 150)
  --duration <s>  simulated seconds (default 600)
  --seed <n>      fixture phase seed (default 20260918)

  both:
  --port <n>      TCP port, 0 for an ephemeral one (default 8787)
  --host <addr>   bind address (default 127.0.0.1; a non-loopback bind needs --token)
  --token <t>     bearer token required on the upgrade and every HTTP request
  --speed <x>     multiple of real time; 0 is unthrottled (default 1)
  --paused        start the run paused at t = 0
  --label <s>     human label for Hello.str_run_label
  --quiet         do not print the banner
  --help          this message
";

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut options = ServerOptions::default();
    let mut stub = StubOptions::default();
    let mut live = LiveOptions::default();
    let mut scenario: Option<String> = None;
    let mut host: IpAddr = [127, 0, 0, 1].into();
    let mut port: u16 = 8787;
    let mut quiet = false;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let mut next = || -> Option<String> {
            i += 1;
            argv.get(i).cloned()
        };
        let parsed = match arg {
            "--help" | "-h" => {
                print!("{USAGE}");
                return std::process::ExitCode::SUCCESS;
            }
            "--quiet" => {
                quiet = true;
                Ok(())
            }
            "--paused" => {
                stub.paused = true;
                live.paused = true;
                Ok(())
            }
            "--scenario" => next()
                .map(|v| scenario = Some(v))
                .ok_or("--scenario needs a path"),
            "--record" => next()
                .map(|v| live.recording = Some(std::path::PathBuf::from(v)))
                .ok_or("--record needs a path"),
            "--build-utc" => next()
                .map(|v| live.build_utc = v)
                .ok_or("--build-utc needs a timestamp"),
            "--retain" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| live.retain_steps = v)
                .ok_or("--retain needs a number"),
            "--lookahead" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| live.lookahead_steps = v)
                .ok_or("--lookahead needs a number"),
            "--speed" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| live.speed = v)
                .ok_or("--speed needs a number"),
            "--label" => next()
                .map(|v| {
                    stub.label = v.clone();
                    live.label = v;
                })
                .ok_or("--label needs a value"),
            "--actors" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| stub.actors = v)
                .ok_or("--actors needs a number"),
            "--grid" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| stub.grid = v)
                .ok_or("--grid needs a number"),
            "--block" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| stub.block_m = v)
                .ok_or("--block needs a number"),
            "--duration" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| stub.duration_s = v)
                .ok_or("--duration needs a number"),
            "--seed" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| stub.seed = v)
                .ok_or("--seed needs a number"),
            "--port" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| port = v)
                .ok_or("--port needs a number"),
            "--host" => next()
                .and_then(|v| v.parse().ok())
                .map(|v| host = v)
                .ok_or("--host needs an IP address"),
            "--token" => next()
                .map(|v| options.token = Some(v))
                .ok_or("--token needs a value"),
            other => Err(if other.starts_with("--") {
                "unknown option"
            } else {
                "unexpected argument"
            }),
        };
        if let Err(message) = parsed {
            eprintln!("{message}: {arg}");
            return std::process::ExitCode::FAILURE;
        }
        i += 1;
    }
    options.bind = SocketAddr::new(host, port);

    let started = match &scenario {
        Some(path) => serve_scenario(options, path, live).await,
        None => serve_stub(options, stub).await,
    };
    let server = match started {
        Ok(s) => s,
        Err(e) => {
            eprintln!("v2xw-server: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if !quiet {
        let run = server.run();
        let d = run.descriptor();
        let (actors, nodes) = run.counts();
        let hash = run.world().content_hash_hex();
        println!("v2xw-server listening on {}", server.http_url());
        println!(
            "  stream   {}?compress=none&v=1   (subprotocol vwp.v1)",
            server.ws_url()
        );
        println!("  world    {}/world/{hash}.vwb", server.http_url());
        println!("  json     {}/world/{hash}.json", server.http_url());
        println!(
            "  rpc      POST {}/rpc   schema {}/rpc/schema",
            server.http_url(),
            server.http_url()
        );
        println!("  run      {}", d.run_id);
        println!(
            "  source   {}",
            scenario
                .as_deref()
                .unwrap_or("synthetic fixture (no --scenario)")
        );
        println!("  scenario {}", d.scenario_hash_hex);
        if let Some(path) = &d.recording_path {
            println!("  record   {path}");
        }
        println!("  traffic  {actors} actors, {nodes} nodes");
        // The banner is the readiness signal a test harness waits on, so it must be
        // flushed before the process starts serving quietly.
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    let _ = tokio::signal::ctrl_c().await;
    server.stop().await;
    std::process::ExitCode::SUCCESS
}
