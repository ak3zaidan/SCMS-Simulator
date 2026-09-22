//! Provisioning inside a running simulation: it queues, it costs time, and it is recorded.
//!
//! The question these tests answer is not "does the flow work" — `tests/flows.rs` answers
//! that — but "does a vehicle that starts up in a simulation pay for its credentials". That
//! needs three things to be true at once, and each is a test here:
//!
//! * the flow is driven by a foreign clock in steps, and a round trip left in flight at the
//!   end of a step is still in flight at the start of the next;
//! * the backend entities queue, so the hundredth vehicle waits behind the ninety-ninth;
//! * the stage stamps land on `proto.revocation` and the hops on `proto.msg` as MCAP
//!   channels a reader can find, which is what makes the decomposition available to
//!   anything other than a Rust test.

mod common;

use std::collections::BTreeMap;

use common::DEVICE_A;
use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, NS_PER_MS, NS_PER_S, SimTime};
use v2xw_proto::net::Transport;
use v2xw_proto::scms::params::ScmsParams;
use v2xw_proto::stage::{FlowId, StageId};
use v2xw_proto::{CredentialService, PseudonymStrategy};

/// The Phase 1 scenario's own shape: 100 ms engine steps over 60 s of simulated time.
const STEP: Duration = Duration::from_millis(100);
/// The scenario's `time.t0` as an offset — a run does not start its clock at zero, and a
/// vehicle that spawns 1.5 s in must ask for credentials at 1.5 s.
const SPAWN: SimTime = 1_500 * NS_PER_MS;

/// A service with the short shuffle window, so a 60-second run can complete a flow.
///
/// The CAMP shuffle is "10,000 requests or one day" [CAMP-EE §2.2.7] and a 60-second
/// scenario cannot wait a day. `ScmsParams::quick` shortens *only* the two batching
/// windows and leaves every cited number alone, which is the difference between a test
/// configuring a deployment and a test editing a citation.
fn service(t0: SimTime) -> CredentialService {
    CredentialService::new(ScmsParams::default().quick(), t0).expect("certificates encode")
}

/// Drives `service` from `t0` to `until` in `STEP` steps, the way an engine's event loop
/// would, collecting everything it produced.
fn drive(service: &mut CredentialService, t0: SimTime, until: SimTime) -> Vec<(SimTime, usize)> {
    let mut per_step = Vec::new();
    let mut now = t0;
    while now <= until {
        service.advance_to(now).expect("backend runs");
        let d = service.drain();
        if !d.is_empty() {
            per_step.push((now, d.len()));
        }
        now = STEP.after(now);
    }
    per_step
}

// -----------------------------------------------------------------------------------------
// 1. It is driven by the engine's clock, in steps
// -----------------------------------------------------------------------------------------

/// A bootstrap spread over engine steps produces the same result as one run to quiescence,
/// and produces it *gradually*.
///
/// The second half is the point. A flow that completed inside one step would be a flow
/// that cost no simulated time, and the way that failure looks from outside is "every
/// record appeared at the same instant".
#[test]
fn a_bootstrap_driven_in_engine_steps_costs_simulated_time() {
    let mut svc = service(SPAWN);
    let b = svc.bootstrap(DEVICE_A, SPAWN, PseudonymStrategy::default());
    let steps = drive(&mut svc, SPAWN, SPAWN + 300 * NS_PER_S);

    let cost = svc
        .provisioning_cost(b.provisioning)
        .expect("the flow completed");
    assert!(
        cost.requested >= SPAWN,
        "the request cannot predate the spawn: {} < {SPAWN}",
        cost.requested
    );
    assert!(
        cost.total().as_nanos() > 0,
        "provisioning must take simulated time"
    );
    assert!(
        steps.len() > 1,
        "the flow must span more than one engine step; it produced records in {} step(s)",
        steps.len()
    );

    // And the credentials really arrived: one i-period of `certs_per_period`.
    let pool = svc.installed(DEVICE_A, cost.installed);
    assert_eq!(pool.len() as u32, svc.params().certs_per_period);
    assert!(pool.iter().all(|p| p.cert_bytes > 0));
}

/// Every declared provisioning stage is stamped, in order, and the decomposition adds up.
#[test]
fn the_provisioning_decomposition_is_complete_and_ordered() {
    let mut svc = service(SPAWN);
    let b = svc.bootstrap(DEVICE_A, SPAWN, PseudonymStrategy::default());
    drive(&mut svc, SPAWN, SPAWN + 300 * NS_PER_S);

    let declared = v2xw_proto::scms::FLOWS
        .iter()
        .find(|f| f.id == FlowId::Provisioning)
        .map(|f| f.stages)
        .expect("declared");
    let seen: Vec<StageId> = svc
        .decomposition(b.provisioning)
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert_eq!(seen, declared, "invariant I-P4, through the engine seam");

    let d = svc.decomposition(b.provisioning);
    assert!(
        d.windows(2).all(|w| w[0].1 <= w[1].1),
        "stage instants must be non-decreasing: {d:?}"
    );
    // The pieces sum to the whole, because they are differences of the same instants.
    let cost = svc.provisioning_cost(b.provisioning).expect("complete");
    let summed: u64 = d.windows(2).map(|w| w[1].1 - w[0].1).sum();
    assert_eq!(summed, cost.total().as_nanos());

    println!("\n=== provisioning decomposition, one vehicle, {SPAWN} ns spawn ===");
    println!("  {:<20} {:>14} {:>12}", "stage", "t (ns)", "delta");
    let mut prev: Option<SimTime> = None;
    for (stage, t) in &d {
        let delta = prev.map_or(String::from("-"), |p| format!("{} us", (t - p) / 1_000));
        println!("  {:<20} {t:>14} {delta:>12}", stage.as_str());
        prev = Some(*t);
    }
    println!(
        "  total requested -> installed: {} us  ({} ms)",
        cost.total().as_nanos() / 1_000,
        cost.total().as_nanos() / 1_000_000
    );
    println!("  hops: {}, bytes: {}", cost.hops, cost.bytes());
    for (transport, bytes) in &cost.bytes_by_transport {
        println!("    {:<14} {bytes:>8} B", transport.as_str());
    }
    println!(
        "  credentials installed: {}",
        svc.installed(DEVICE_A, cost.installed).len()
    );
}

/// The device's clock is not the backend's: its credentials are valid over the i-period
/// its start-up instant falls in, not over "the run".
#[test]
fn the_credentials_validity_window_comes_from_the_i_period_the_spawn_falls_in() {
    let params = ScmsParams::default().quick();
    let mid_period = params.i_period.as_nanos() * 3 + 7 * NS_PER_S;
    let mut svc = CredentialService::new(params, mid_period).expect("encodes");
    let b = svc.bootstrap(DEVICE_A, mid_period, PseudonymStrategy::default());
    drive(&mut svc, mid_period, mid_period + 300 * NS_PER_S);

    let cost = svc.provisioning_cost(b.provisioning).expect("complete");
    let pool = svc.installed(DEVICE_A, cost.installed);
    assert!(!pool.is_empty());
    for p in &pool {
        assert_eq!(p.i_period, 3, "the spawn is inside i-period 3");
        assert_eq!((p.valid_from, p.valid_until), params.validity(3));
        assert!(p.usable, "a just-installed credential must be usable");
    }
}

// -----------------------------------------------------------------------------------------
// 2. The backend entities queue
// -----------------------------------------------------------------------------------------

/// A fleet starting up together makes the backend wait, and the waiting is visible.
///
/// The injected fault this test is protecting against is a backend whose service time is
/// charged but whose queue is never contended — which is what an entity modelled as a
/// function call looks like. With 4 servers and 64 vehicles arriving at once, the queue
/// must have accumulated waiting time at the entity that does the most work per request.
#[test]
fn a_fleet_starting_together_makes_the_backend_queue() {
    const FLEET: u32 = 64;
    let mut svc = service(SPAWN);
    for k in 0..FLEET {
        svc.bootstrap(NodeId::new(1_000 + k), SPAWN, PseudonymStrategy::default());
    }
    drive(&mut svc, SPAWN, SPAWN + 600 * NS_PER_S);

    let nodes = svc.deployment().state.nodes;
    let kernel = &svc.deployment().kernel;
    let pca = kernel.queue(nodes.pca).expect("the PCA is hosted");
    let ra = kernel.queue(nodes.ra).expect("the RA is hosted");

    assert_eq!(
        pca.served(),
        u64::from(FLEET),
        "one certification request per vehicle"
    );
    assert!(
        pca.waiting().as_nanos() > 0,
        "{FLEET} vehicles on {} servers must produce queueing at the PCA",
        svc.params().backend_servers
    );
    assert!(
        ra.busy().as_nanos() > 0,
        "the Registration Authority must have done work"
    );

    // And the fleet's last vehicle finished later than its first, because it waited.
    let first = svc
        .provisioning_cost(
            svc.bootstrap_of(NodeId::new(1_000))
                .expect("bootstrapped")
                .provisioning,
        )
        .expect("complete");
    let last = svc
        .provisioning_cost(
            svc.bootstrap_of(NodeId::new(1_000 + FLEET - 1))
                .expect("bootstrapped")
                .provisioning,
        )
        .expect("complete");
    assert!(
        last.installed > first.installed,
        "the last vehicle of a fleet must be provisioned after the first: {} vs {}",
        last.installed,
        first.installed
    );

    println!("\n=== backend under a {FLEET}-vehicle simultaneous start-up ===");
    println!(
        "  {:<12} {:>8} {:>14} {:>14}",
        "entity", "served", "busy (us)", "waited (us)"
    );
    for (name, node) in [
        ("LOP", nodes.lop),
        ("RA", nodes.ra),
        ("PCA", nodes.pca),
        ("LA1", nodes.la1),
        ("LA2", nodes.la2),
        ("DCM", nodes.dcm),
        ("ECA", nodes.eca),
    ] {
        let q = kernel.queue(node).expect("hosted");
        println!(
            "  {name:<12} {:>8} {:>14} {:>14}",
            q.served(),
            q.busy().as_nanos() / 1_000,
            q.waiting().as_nanos() / 1_000
        );
    }
    println!(
        "  first vehicle installed at {} ns, last at {} ns, spread {} us",
        first.installed,
        last.installed,
        (last.installed - first.installed) / 1_000
    );
    println!(
        "  bytes for the whole fleet: {} B over {} hops",
        kernel.bytes_by_transport().values().sum::<u64>(),
        kernel.steps.len()
    );
}

/// Provisioning costs bytes, on the transports it really crosses.
#[test]
fn provisioning_costs_bytes_on_both_the_uplink_and_the_backend_network() {
    let mut svc = service(SPAWN);
    let b = svc.bootstrap(DEVICE_A, SPAWN, PseudonymStrategy::default());
    drive(&mut svc, SPAWN, SPAWN + 300 * NS_PER_S);

    let cost = svc.provisioning_cost(b.provisioning).expect("complete");
    let uu = cost
        .bytes_by_transport
        .get(&Transport::CellularUu)
        .copied()
        .unwrap_or(0);
    let backend = cost
        .bytes_by_transport
        .get(&Transport::BackendNet)
        .copied()
        .unwrap_or(0);
    assert!(uu > 0, "the device's own uplink must carry bytes");
    assert!(backend > 0, "the backend network must carry bytes");
    assert!(
        backend > uu,
        "the pre-linkage and certification exchanges dominate: backend {backend} B vs uplink {uu} B"
    );
    assert!(
        cost.hops >= 8,
        "the flow is at least eight hops: {}",
        cost.hops
    );
}

// -----------------------------------------------------------------------------------------
// 3. It reaches the recording
// -----------------------------------------------------------------------------------------

/// The stage stamps and the hops are recordable, on the channels 03-interfaces §14 names,
/// and their JSON carries the whole provisioning decomposition.
///
/// This is the engine seam's recording half, tested at the only place this crate owns: a
/// record is a `(channel, visibility, JSON)` triple, and a recorder writes it. What is
/// asserted here is the triple — that `proto.revocation` carries every declared stage with
/// its instant, that `proto.msg` carries every hop with its bytes and its transport, and
/// that both serialise — because that is exactly what the engine's recorder is handed.
#[test]
fn every_record_the_service_emits_is_recordable_and_carries_the_decomposition() {
    use v2xw_core::ctx::{ErasedRecord, Visibility};

    let mut svc = service(SPAWN);
    let b = svc.bootstrap(DEVICE_A, SPAWN, PseudonymStrategy::default());

    let mut owned: Vec<v2xw_core::ctx::OwnedRecord> = Vec::new();
    let mut now = SPAWN;
    let end = SPAWN + 300 * NS_PER_S;
    while now <= end {
        svc.advance_to(now).expect("backend runs");
        let d = svc.drain();
        for s in &d.stages {
            owned.push(s.to_owned_record().expect("a stage stamp serialises"));
        }
        for s in &d.steps {
            owned.push(s.to_owned_record().expect("a wire step serialises"));
        }
        for c in &d.certs {
            owned.push(c.to_owned_record().expect("a cert event serialises"));
        }
        now = STEP.after(now);
    }

    let mut per_channel: BTreeMap<&'static str, u64> = BTreeMap::new();
    for r in &owned {
        *per_channel.entry(r.channel).or_insert(0) += 1;
    }
    assert_eq!(
        per_channel.keys().copied().collect::<Vec<_>>(),
        vec!["proto.msg", "proto.revocation"],
        "a provisioning-only run touches exactly these two channels"
    );

    // Visibility: 03-interfaces §14 gives `proto.revocation` as PUBLIC and `proto.msg` as
    // NODE. A stage stamp tagged GT would be a leak, and the recorder would refuse it.
    for r in &owned {
        let expected = match r.channel {
            "proto.revocation" => Visibility::Public,
            "proto.msg" => Visibility::Node,
            other => panic!("unexpected channel {other}"),
        };
        assert_eq!(r.visibility, expected, "{}", r.channel);
        assert!(
            !r.visibility.is_gt_tainted(),
            "{} must not be GT",
            r.channel
        );
    }

    // And the stages that landed on `proto.revocation` are the declared decomposition, in
    // order, read out of the serialised JSON rather than out of the log it came from.
    let mut stages_in_json: Vec<String> = Vec::new();
    for r in owned.iter().filter(|r| r.channel == "proto.revocation") {
        let v: serde_json::Value = serde_json::from_slice(&r.json).expect("json");
        if v["flow"] == "provisioning" {
            assert!(v["t"].is_u64(), "every stamp carries its instant");
            stages_in_json.push(v["stage"].as_str().expect("a stage name").to_string());
        }
    }
    let expected: Vec<String> = v2xw_proto::scms::FLOWS
        .iter()
        .find(|f| f.id == FlowId::Provisioning)
        .map(|f| f.stages.iter().map(|s| s.as_str().to_string()).collect())
        .expect("declared");
    assert_eq!(
        stages_in_json, expected,
        "the serialised records must carry the whole provisioning decomposition, in order"
    );

    // `proto.msg`'s documented key fields are `t, from, to, flow, step, bytes, transport`.
    for r in owned.iter().filter(|r| r.channel == "proto.msg") {
        let v: serde_json::Value = serde_json::from_slice(&r.json).expect("json");
        for field in ["t", "from", "to", "flow", "step", "bytes", "transport"] {
            assert!(!v[field].is_null(), "proto.msg is missing `{field}`: {v}");
        }
        assert!(
            v["bytes"].as_u64().is_some_and(|b| b > 0),
            "a hop with no bytes: {v}"
        );
    }

    assert!(
        svc.provisioning_cost(b.provisioning).is_some(),
        "and the decomposition recorded is the completed one"
    );
}
