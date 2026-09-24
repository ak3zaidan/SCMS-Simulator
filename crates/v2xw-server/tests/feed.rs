//! The followed vehicle's message feed and queues, end to end through a real kernel.
//!
//! The owner's ask: in the chase view, see the broadcasts a vehicle sends with their content,
//! and its queue. These tests drive a real grid run and check that what the feed says is
//! what the octets say and what the vehicle is doing: a BSM decoded from the SPDU the node
//! signed carries the vehicle's own position, speed and heading; a received message names
//! its sender and decodes to the sender's content; the queues are the node's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::Vec3;
use v2xw_server::feed::{FEED_VERSION, FeedLimits};
use v2xw_server::live::{LiveEngine, LiveOptions};
use v2xw_server::rpc::{self, Context};
use v2xw_server::{Run, Session};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

fn scenario(name: &str, duration_s: f64) -> PathBuf {
    let text = std::fs::read_to_string(repo_root().join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("duration_s: 60.0", &format!("duration_s: {duration_s:.1}"))
        .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 20000.0");
    let dir = std::env::temp_dir().join("v2xw-server-feed");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(format!("{name}.yaml"));
    std::fs::write(&path, text).expect("write scenario");
    path
}

fn call(
    run: &Run,
    session: Option<&mut Session>,
    method: &str,
    params: Value,
) -> Result<rpc::Outcome, Value> {
    let text = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = rpc::parse(&text).expect("parse");
    let mut ctx = Context {
        run,
        session,
        pending: None,
        received_at: None,
    };
    rpc::dispatch(&mut ctx, &request).map_err(|e| rpc::failure(&json!(1), &e)["error"].clone())
}

/// One vehicle's pose at one step.
#[derive(Debug, Clone, Copy)]
struct Pose {
    t: u64,
    pos: [f64; 3],
    speed: f64,
    heading_rad: f64,
}

/// A run advanced to `until_ns`, with every node's poses along the way.
fn advance(name: &str, until_ns: u64) -> (std::sync::Arc<Run>, BTreeMap<u32, Vec<Pose>>) {
    let engine = LiveEngine::open(
        scenario(name, 8.0),
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
    call(&run, None, "run.start", json!({"paused": true, "speed": 0})).expect("start");
    call(&run, None, "run.resume", json!({})).expect("resume");
    let mut steps = run.subscribe();
    let mut poses: BTreeMap<u32, Vec<Pose>> = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(300);
    while run.sim_time() < until_ns {
        assert!(
            Instant::now() < deadline,
            "the run did not reach {until_ns} ns"
        );
        if !run.tick().expect("the engine runs") {
            std::thread::sleep(Duration::from_millis(5));
        }
        while let Ok(step) = steps.try_recv() {
            for a in &step.snapshot.actors {
                if let Some(node) = a.node {
                    poses.entry(node.index()).or_default().push(Pose {
                        t: step.sim_time,
                        pos: a.pos_m,
                        speed: a.speed_mps,
                        heading_rad: a.heading_rad,
                    });
                }
            }
        }
    }
    (run, poses)
}

fn field<'a>(fields: &'a Value, key: &str) -> &'a Value {
    fields
        .as_array()
        .and_then(|a| a.iter().find(|f| f["k"] == key))
        .unwrap_or_else(|| panic!("no decoded field `{key}` in {fields}"))
}

fn num(fields: &Value, key: &str) -> f64 {
    field(fields, key)["v"]
        .as_f64()
        .unwrap_or_else(|| panic!("`{key}` is not a number: {}", field(fields, key)))
}

/// J2735's heading, degrees clockwise from true north, for an ENU heading.
fn bearing_deg(heading_rad: f64) -> f64 {
    (90.0 - heading_rad.to_degrees()).rem_euclid(360.0)
}

fn angle_gap(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

#[test]
fn a_sent_bsm_decodes_from_its_octets_to_the_vehicles_own_pose() {
    let (run, poses) = advance("sent", 4_000_000_000);
    let hello = run.descriptor().hello.clone();
    let origin = GeoOrigin::new(
        hello.origin_lat_deg,
        hello.origin_lon_deg,
        hello.origin_alt_m,
    );

    let mut checked = 0usize;
    let mut gaps: Vec<f64> = Vec::new();
    for (node, track) in &poses {
        let Some(feed) = run.node_feed(*node, None, &FeedLimits::default(), true) else {
            panic!("the live engine has no feed for node {node}");
        };
        assert_eq!(feed["v"], json!(FEED_VERSION));
        for sent in feed["sent"].as_array().expect("sent") {
            assert_eq!(sent["type"], "bsm", "the grid run sends BSMs: {sent}");
            let d = &sent["decoded"];
            assert!(d.get("error").is_none(), "the SPDU did not decode: {d}");
            // The octets and their layers agree with the record.
            let spdu = d["spdu_bytes"].as_u64().expect("spdu size");
            let payload = sent["bytes"]["payload"].as_u64().expect("payload octets");
            let envelope = sent["bytes"]["envelope"].as_u64().expect("envelope octets");
            assert_eq!(
                spdu,
                payload + envelope,
                "SPDU = payload + envelope: {sent}"
            );
            assert_eq!(d["hex"].as_str().map(str::len), Some(2 * spdu as usize));
            assert_eq!(d["security"]["payload_bytes"].as_u64(), Some(payload));
            // The spans tile the SPDU, in order, and the payload span is the payload.
            let spans = d["spans"].as_array().expect("spans");
            let mut at = 0;
            for s in spans {
                assert_eq!(s["start"].as_u64(), Some(at), "spans must tile: {spans:?}");
                at = s["end"].as_u64().expect("end");
            }
            assert_eq!(at, spdu, "the spans must end at the SPDU's end");
            let p = spans
                .iter()
                .find(|s| s["layer"] == "payload")
                .expect("a payload span");
            assert_eq!(
                p["end"].as_u64().unwrap() - p["start"].as_u64().unwrap(),
                payload
            );
            assert!(spans.iter().any(|s| s["name"] == "signature"), "{spans:?}");
            // The signer the envelope names is the pseudonym the record names.
            assert_eq!(
                d["security"]["signer"]["hashed_id8"], sent["pseudonym"],
                "the SPDU's signer must be the frame's pseudonym: {sent}"
            );
            // The temporary id is the pseudonym's first four octets (J2945/1 §6.2.1 practice).
            let fields = &d["message"]["fields"];
            let temp_id = field(fields, "temp_id")["v"].as_str().expect("temp id");
            assert!(sent["pseudonym"].as_str().unwrap().starts_with(temp_id));

            // What the octets say, against what the vehicle was doing when it said it.
            let t_gen = sent["timing"]["generated_ns"].as_u64().expect("generation");
            let Some(pose) = track.iter().rev().find(|p| p.t <= t_gen).copied() else {
                continue;
            };
            let lat = num(fields, "lat");
            let lon = num(fields, "lon");
            let at_enu = origin.to_enu(lat, lon, 0.0);
            let gap = Vec3::new(pose.pos[0], pose.pos[1], 0.0).distance_2d(at_enu);
            // The BSM carries the node's GNSS belief of its reference point, and the stream
            // draws the body centre half a car ahead of it. The belief is the Gauss-Markov
            // model's (`v2xw_mobility::gnss`): a few metres of correlated error with 1 %
            // outliers of 12 m and bursts at six times the noise, so one frame may legitimately
            // sit 20 m off. The claim is about the distribution, checked after the loop.
            gaps.push(gap);
            let speed = num(fields, "speed");
            assert!(
                (speed - pose.speed).abs() < 2.0,
                "node {node}: BSM speed {speed} vs the vehicle's {}",
                pose.speed
            );
            if pose.speed > 2.0 {
                let heading = num(fields, "heading");
                let gap = angle_gap(heading, bearing_deg(pose.heading_rad));
                assert!(
                    gap < 15.0,
                    "node {node}: BSM heading {heading} vs {}",
                    bearing_deg(pose.heading_rad)
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 20,
        "only {checked} BSMs were checked against a pose"
    );
    gaps.sort_by(f64::total_cmp);
    let median = gaps[gaps.len() / 2];
    let p90 = gaps[gaps.len() * 9 / 10];
    // Blocks here are 80 m by 274 m and cars are metres apart in a queue: a BSM decoded to
    // another vehicle, or read at the wrong scale, lands tens to hundreds of metres away.
    assert!(
        median < 8.0,
        "median BSM-to-vehicle distance {median:.1} m over {checked} frames"
    );
    assert!(
        p90 < 25.0,
        "90th percentile BSM-to-vehicle distance {p90:.1} m over {checked} frames"
    );
}

#[test]
fn received_messages_name_their_sender_and_decode_to_its_content() {
    let (run, _) = advance("received", 4_000_000_000);
    let nodes: Vec<u32> = run.nodes().iter().map(|r| r.node_id).collect();
    let mut delivered = 0usize;
    for node in nodes {
        let feed = run
            .node_feed(
                node,
                None,
                &FeedLimits {
                    received: 500,
                    ..FeedLimits::default()
                },
                true,
            )
            .expect("feed");
        for r in feed["received"].as_array().expect("received") {
            let from = r["from"].as_u64().expect("a sender") as u32;
            assert_ne!(from, node, "a node does not hear itself");
            assert!(["delivered", "lost", "in-flight"].contains(&r["outcome"].as_str().unwrap()));
            if r["outcome"] == "lost" {
                assert!(r["cause"].is_string(), "a loss has a cause: {r}");
                assert!(
                    !["out-of-range", "below-sensitivity"].contains(&r["cause"].as_str().unwrap()),
                    "an undetected frame is counted, not listed: {r}"
                );
                assert!(
                    r.get("decoded").is_none(),
                    "a lost frame's octets never reached the receiver: {r}"
                );
                continue;
            }
            if r["outcome"] != "delivered" {
                continue;
            }
            // The content is the sender's: its temporary id is the sender's pseudonym prefix.
            let d = &r["decoded"];
            let temp = field(&d["message"]["fields"], "temp_id")["v"]
                .as_str()
                .expect("temp id")
                .to_string();
            let sender = run
                .node_feed(
                    from,
                    None,
                    &FeedLimits {
                        sent: 200,
                        ..FeedLimits::default()
                    },
                    true,
                )
                .unwrap();
            let msg = r["msg"].as_u64().expect("msg id");
            let tx = sender["sent"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["msg"].as_u64() == Some(msg))
                .unwrap_or_else(|| panic!("node {from} has no frame {msg}"));
            assert!(tx["pseudonym"].as_str().unwrap().starts_with(&temp));
            // The delay decomposition tiles the end-to-end delay.
            let e2e = r["e2e_ms"].as_f64().expect("e2e");
            let sum: f64 = r["stages_ms"]
                .as_object()
                .unwrap()
                .values()
                .filter_map(Value::as_f64)
                .sum();
            assert!(
                (sum - e2e).abs() < 1e-3,
                "stages {sum} ms vs e2e {e2e} ms: {r}"
            );
            assert!(r["rssi_dbm"].is_number() && r["dist_m"].is_number(), "{r}");
            delivered += 1;
        }
    }
    assert!(delivered >= 20, "only {delivered} deliveries were checked");
}

#[test]
fn the_feed_is_bounded_and_says_what_it_left_out() {
    let (run, _) = advance("bounded", 3_000_000_000);
    let node = run.nodes().first().map(|r| r.node_id).expect("a node");
    let all = run
        .node_feed(
            node,
            None,
            &FeedLimits {
                sent: 200,
                received: 500,
                waiting: 100,
                bytes: false,
            },
            true,
        )
        .unwrap();
    let total_sent = all["sent"].as_array().unwrap().len();
    assert!(total_sent >= 5, "node {node} sent {total_sent}");
    assert!(
        all["sent"][0]["decoded"].get("hex").is_none(),
        "bytes: false carries no hex"
    );
    let one = run
        .node_feed(
            node,
            None,
            &FeedLimits {
                sent: 1,
                received: 1,
                waiting: 1,
                bytes: true,
            },
            true,
        )
        .unwrap();
    assert_eq!(one["sent"].as_array().unwrap().len(), 1);
    assert_eq!(one["omitted"]["sent"].as_u64(), Some(total_sent as u64 - 1));
    // Newest first.
    assert_eq!(one["sent"][0]["msg"], all["sent"][0]["msg"]);
    // A push after the last one carries only what is new.
    let t = all["t_ns"].as_u64().unwrap();
    let again = run
        .node_feed(node, Some(t), &FeedLimits::default(), true)
        .unwrap();
    assert_eq!(again["sent"].as_array().unwrap().len(), 0, "{again}");
    assert_eq!(again["reset"], json!(false));
    // A cursor ahead of the stream (a backward seek) starts over.
    let reset = run
        .node_feed(node, Some(t + 1_000_000_000), &FeedLimits::default(), true)
        .unwrap();
    assert_eq!(reset["reset"], json!(true));
}

#[test]
fn the_queues_are_the_five_of_the_node_model_with_their_waits() {
    let (run, _) = advance("queues", 3_000_000_000);
    let mut served = 0u64;
    for row in run.nodes() {
        let feed = run
            .node_feed(row.node_id, None, &FeedLimits::default(), true)
            .unwrap();
        let list = feed["queues"]["list"].as_array().expect("queues");
        let ids: Vec<&str> = list.iter().map(|q| q["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["rx", "verify", "app", "tx", "crl"]);
        for q in list {
            if q["id"] != "crl" {
                // `waiting` is every message that waited during the last step: those still
                // there (`left_ns` null, as many as `depth`) and those that left during it.
                let depth = q["depth"].as_u64().expect("a depth");
                let peak = q["peak"].as_u64().expect("a peak");
                let rows = q["waiting"].as_array().unwrap();
                let total = rows.len() as u64 + q["waiting_omitted"].as_u64().unwrap();
                assert!(depth <= peak && peak <= total.max(depth), "{q}");
                if q["waiting_omitted"] == 0 {
                    let still = rows.iter().filter(|w| w["left_ns"].is_null()).count() as u64;
                    assert_eq!(still, depth, "{q}");
                }
                let now = feed["t_ns"].as_u64().unwrap();
                let step = (feed["queues"]["step_ms"].as_f64().unwrap() * 1e6) as u64;
                for w in rows {
                    assert!(w["waited_ms"].as_f64().unwrap() >= 0.0);
                    assert!(w["enqueued_ns"].as_u64().unwrap() <= now);
                    if let Some(left) = w["left_ns"].as_u64() {
                        assert!(
                            left <= now && left > now - step,
                            "left outside the step: {w}"
                        );
                    }
                }
            }
            served += q["served"].as_u64().unwrap();
            if let (Some(p50), Some(p95)) = (q["wait_p50_ms"].as_f64(), q["wait_p95_ms"].as_f64()) {
                assert!(p50 <= p95 && p50 >= 0.0, "{q}");
            }
        }
    }
    assert!(
        served > 0,
        "no queue served anything in a second of a 3000 veh/h run"
    );
}

#[test]
fn view_follow_with_a_feed_pushes_it_and_a_new_follow_drops_it() {
    let (run, _) = advance("follow", 3_000_000_000);
    let descriptor = run.descriptor();
    let nodes: Vec<u32> = run.nodes().iter().map(|r| r.node_id).collect();
    assert!(nodes.len() >= 2);
    let mut session = Session::new(Default::default(), &descriptor);
    session.bind(run.generation(), "");
    let out = call(
        &run,
        Some(&mut session),
        "view.follow",
        json!({"node": nodes[0], "feed": {"sent": 5, "hz": 2}}),
    )
    .expect("view.follow");
    assert_eq!(
        out.result["feed"]["v"],
        json!(FEED_VERSION),
        "{}",
        out.result
    );
    assert_eq!(out.result["feed"]["available"], json!(true));
    assert_eq!(
        out.notifications.len(),
        1,
        "the first push follows the reply"
    );
    let n = &out.notifications[0];
    assert_eq!(n["method"], "node.feed");
    assert_eq!(n["params"]["node"], json!(nodes[0]));
    assert!(n["params"]["sent"].as_array().unwrap().len() <= 5);
    let sub = session.feed().expect("subscribed").clone();
    assert_eq!((sub.node, sub.hz, sub.limits.sent), (nodes[0], 2.0, 5));
    // Following another vehicle drops the old node's feed: it is never pushed for a car
    // that is not the one on screen.
    call(
        &run,
        Some(&mut session),
        "view.follow",
        json!({"node": nodes[1]}),
    )
    .expect("follow");
    assert!(session.feed().is_none());
    // A bad option is refused with its path.
    let err = call(
        &run,
        Some(&mut session),
        "view.follow",
        json!({"node": nodes[1], "feed": {"hz": 99}}),
    )
    .err()
    .expect("refused");
    assert_eq!(err["code"], json!(-32602), "{err}");
    // `clear` drops it too.
    call(
        &run,
        Some(&mut session),
        "view.follow",
        json!({"node": nodes[1], "feed": true}),
    )
    .expect("follow");
    assert!(session.feed().is_some());
    call(
        &run,
        Some(&mut session),
        "view.follow",
        json!({"clear": true}),
    )
    .expect("clear");
    assert!(session.feed().is_none());
}

/// Every member path of a JSON value, arrays collapsed to `[]`, with its JSON type.
///
/// `null` and a number are the same member for this purpose — an optional quantity is
/// either — so both read as `value`.
fn shape(v: &Value, at: &str, out: &mut std::collections::BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            out.insert(format!("{at}:object"));
            for (k, x) in o {
                shape(x, &format!("{at}.{k}"), out);
            }
        }
        Value::Array(a) => {
            out.insert(format!("{at}:array"));
            if let Some(first) = a.first() {
                shape(first, &format!("{at}[]"), out);
            }
        }
        Value::String(_) => {
            out.insert(format!("{at}:string"));
        }
        Value::Bool(_) => {
            out.insert(format!("{at}:bool"));
        }
        Value::Number(_) | Value::Null => {
            out.insert(format!("{at}:value"));
        }
    }
}

/// The parts of a push whose shape is fixed: the envelope, one sent frame, one delivered
/// reception, one queue and one waiting entry.
fn feed_shape(feed: &Value) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    // Members that are present or not by the frame, not by the schema: a certificate is
    // attached to some frames and a digest names the signer on the rest (§6.3 of 1609.2),
    // and a generation location is optional in `headerInfo`.
    let mut feed = feed.clone();
    for entry in ["sent", "received"] {
        if let Some(rows) = feed[entry].as_array_mut() {
            for row in rows {
                if let Some(sec) = row
                    .pointer_mut("/decoded/security")
                    .and_then(Value::as_object_mut)
                {
                    sec.remove("generation_location");
                    if let Some(signer) = sec.get_mut("signer").and_then(Value::as_object_mut) {
                        signer.remove("certificate");
                        signer.insert("kind".into(), json!("digest|certificate"));
                    }
                }
            }
        }
    }
    let feed = &feed;
    let mut top = feed.clone();
    let obj = top.as_object_mut().expect("an object");
    for k in ["sent", "received", "queues"] {
        obj.remove(k);
    }
    shape(&top, "", &mut out);
    shape(&feed["sent"][0], "sent[]", &mut out);
    let delivered = feed["received"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["outcome"] == "delivered"))
        .expect("a delivered reception");
    shape(delivered, "received[]", &mut out);
    let mut queues = feed["queues"].clone();
    let list = queues["list"].take();
    shape(&queues, "queues", &mut out);
    let tx = list
        .as_array()
        .and_then(|l| l.iter().find(|q| q["id"] == "tx"))
        .expect("the tx queue");
    let mut q = tx.clone();
    q["waiting"] = json!([]);
    shape(&q, "queues.list[]", &mut out);
    out
}

/// The shared vector both packages read: `docs/protocol/vectors/node-feed-v1.json`.
///
/// The server's `node.feed` must have exactly the vector's shape, so a member added, renamed
/// or retyped on this side fails here until the vector is re-blessed deliberately (with
/// `VWP_BLESS_FEED_VECTOR=1`), and `@vwp/protocol`'s own test then checks its types against
/// the new vector. That is what keeps the Rust and TypeScript definitions of the feed one
/// schema, and what makes a change to it a decision about [`FEED_VERSION`].
#[test]
fn the_feed_has_the_shape_of_the_published_vector() {
    let (run, _) = advance("vector", 4_000_000_000);
    let limits = FeedLimits {
        sent: 1,
        received: 40,
        waiting: 1,
        bytes: true,
    };
    let feed = run
        .nodes()
        .iter()
        .filter_map(|r| run.node_feed(r.node_id, None, &limits, true))
        .find(|f| {
            f["sent"].as_array().is_some_and(|a| !a.is_empty())
                && f["received"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|r| r["outcome"] == "delivered"))
        })
        .expect("a node that sent and received");
    let path = repo_root().join("docs/protocol/vectors/node-feed-v1.json");
    if std::env::var("VWP_BLESS_FEED_VECTOR").is_ok() {
        // One sent frame and one delivered reception are enough to show every member.
        let mut v = feed.clone();
        let delivered = v["received"]
            .as_array()
            .and_then(|a| a.iter().find(|r| r["outcome"] == "delivered"))
            .cloned()
            .expect("delivered");
        v["received"] = json!([delivered]);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("mkdir");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&v).expect("json") + "\n",
        )
        .expect("write the vector");
    }
    let vector: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("docs/protocol/vectors/node-feed-v1.json"),
    )
    .expect("the vector is JSON");
    assert_eq!(vector["v"], json!(FEED_VERSION), "the vector's version");
    let (want, got) = (feed_shape(&vector), feed_shape(&feed));
    let missing: Vec<&String> = want.difference(&got).collect();
    let extra: Vec<&String> = got.difference(&want).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "node.feed no longer has the published shape.\n  in the vector, not produced: \
         {missing:?}\n  produced, not in the vector: {extra:?}\nRe-bless with \
         VWP_BLESS_FEED_VECTOR=1 only if the change is deliberate, and raise FEED_VERSION if \
         a v1 reader would misread it."
    );
}
