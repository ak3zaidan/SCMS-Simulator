//! True resume (§1.4), over real sockets, through a proxy this test can cut.
//!
//! Before this, a session died with its socket: a reconnect after a brief network drop got
//! a fresh `Hello` and a resync keyframe, the client threw its stream state away, and a
//! page following a car lost the car. These tests cut the TCP connection between client
//! and server mid-run — no WebSocket close frame, exactly what a dropped Wi-Fi link looks
//! like to the server — reconnect with the token the first `Hello` issued, and hold the
//! server to §1.4:
//!
//! * inside the ring, the `Hello` says `HELLO_RESUMED` at the client's `seq`, and what
//!   follows is every frame the client missed, then the live stream, with no `seq` gap, no
//!   duplicate and no resync keyframe;
//! * outside the ring, or without a token, it is a fresh `Hello` and a resync keyframe,
//!   never an error;
//! * a second connection presenting a live session's token supersedes the first, which is
//!   told so with `Bye{reason = 4}` and close 1012;
//! * a clean close (1000) is not resumable.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use v2xw_record::wire::hello::{HELLO_RESUMED, HelloBody};
use v2xw_record::wire::{FLAG_RESYNC, Frame, MsgType};
use v2xw_server::{ServerOptions, StubOptions, VwpServer, serve_stub};

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// A TCP proxy whose open connections can be cut, as a network drop cuts them.
struct Proxy {
    addr: std::net::SocketAddr,
    live: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Proxy {
    async fn to(upstream: std::net::SocketAddr) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
        let addr = listener.local_addr().expect("proxy addr");
        let live: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::default();
        let registry = Arc::clone(&live);
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let task = tokio::spawn(async move {
                    if let Ok(mut outbound) = TcpStream::connect(upstream).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                registry.lock().await.push(task);
            }
        });
        Proxy { addr, live }
    }

    /// Drops every proxied connection on the floor: both TCP halves close with no
    /// WebSocket close frame on either side.
    async fn cut(&self) {
        for task in self.live.lock().await.drain(..) {
            task.abort();
        }
    }
}

async fn server(paused: bool) -> VwpServer {
    serve_stub(
        ServerOptions {
            bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            ..ServerOptions::default()
        },
        StubOptions {
            actors: 24,
            grid: 4,
            duration_s: 600,
            paused,
            // Twenty times real time: fast enough that a half-second drop misses a hundred
            // steps, slow enough that the ring (4096 frames) still holds all of them.
            speed: 20.0,
            ..StubOptions::default()
        },
    )
    .await
    .expect("server")
}

async fn open(addr: std::net::SocketAddr, query: &str) -> Ws {
    let url = format!("ws://{addr}/vwp/v1?compress=none&v=1{query}");
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
            .expect("request");
    request.headers_mut().insert(
        "sec-websocket-protocol",
        "vwp.v1".parse().expect("header value"),
    );
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connect");
    ws
}

/// The next binary frame, skipping text (JSON-RPC notifications) and pings.
async fn next_frame(ws: &mut Ws) -> Option<Frame> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .ok()??
            .ok()?;
        match msg {
            Message::Binary(b) => return Frame::from_bytes(b.to_vec()).ok(),
            Message::Close(_) => return None,
            _ => {}
        }
    }
}

struct Greeting {
    flags: u32,
    resume_seq: u64,
    token: String,
}

async fn hello(ws: &mut Ws) -> Greeting {
    let frame = next_frame(ws).await.expect("a Hello");
    let header = frame.header().expect("header");
    assert_eq!(
        header.msg_type,
        MsgType::Hello.id(),
        "the first frame is Hello"
    );
    let body = HelloBody::decode(frame.body()).expect("decode Hello");
    Greeting {
        flags: body.hello_flags,
        resume_seq: body.resume_seq,
        token: body
            .strings
            .get(body.str_session_token)
            .unwrap_or_default()
            .to_string(),
    }
}

/// `(seq, msg_type, flags, sim_time_ns)` of the next `n` canonical frames. The time is a
/// snapshot's (§3.3.1 and §3.4.1 both open with `sim_time_ns`), zero for anything else.
async fn canonical(ws: &mut Ws, n: usize) -> Vec<(u64, u16, u16, u64)> {
    let mut out = Vec::new();
    while out.len() < n {
        let frame = next_frame(ws).await.expect("a frame");
        let h = frame.header().expect("header");
        if MsgType::from_id(h.msg_type).is_some_and(MsgType::is_canonical) {
            let snapshot =
                h.msg_type == MsgType::Keyframe.id() || h.msg_type == MsgType::Delta.id();
            let t = if snapshot {
                u64::from_le_bytes(frame.body()[0..8].try_into().expect("8 bytes"))
            } else {
                0
            };
            out.push((h.seq, h.msg_type, h.flags, t));
        }
    }
    out
}

fn assert_dense(seqs: &[(u64, u16, u16, u64)], from: u64) {
    for (i, (seq, _, _, _)) in seqs.iter().enumerate() {
        assert_eq!(
            *seq,
            from + i as u64,
            "seq {i} after the resume point: a gap or a duplicate in {seqs:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cut_connection_resumes_with_exactly_the_missed_frames() {
    let server = server(false).await;
    let proxy = Proxy::to(server.address()).await;

    let mut ws = open(proxy.addr, "").await;
    let first = hello(&mut ws).await;
    assert_eq!(
        first.flags & HELLO_RESUMED,
        0,
        "a new connection is not a resume"
    );
    assert_eq!(
        first.token.len(),
        32,
        "Hello issues a session token: {:?}",
        first.token
    );
    let before = canonical(&mut ws, 40).await;
    assert_dense(&before, 0);
    let next = before.last().expect("frames").0 + 1;

    // The drop: TCP torn down under both ends, no close frame. The run keeps going.
    proxy.cut().await;
    drop(ws);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut ws = open(
        proxy.addr,
        &format!("&session={}&resume={next}", first.token),
    )
    .await;
    let again = hello(&mut ws).await;
    assert_ne!(again.flags & HELLO_RESUMED, 0, "HELLO_RESUMED is set");
    assert_eq!(again.resume_seq, next, "resume_seq is the client's seq");
    assert_eq!(again.token, first.token, "the same session");
    // Half a second at twenty times real time is about a hundred steps missed; read well
    // past them into the live stream.
    let after = canonical(&mut ws, 400).await;
    assert_dense(&after, next);
    // Every step reached the client exactly once: the snapshot times run on from the last
    // one it saw before the cut, one mobility step (100 ms) apart, with none missing.
    const STEP_NS: u64 = 100_000_000;
    let snapshot_times = |frames: &[(u64, u16, u16, u64)]| -> Vec<u64> {
        frames
            .iter()
            .filter(|(_, t, _, _)| *t == MsgType::Keyframe.id() || *t == MsgType::Delta.id())
            .map(|(_, _, _, time)| *time)
            .collect()
    };
    let last_seen = *snapshot_times(&before)
        .last()
        .expect("a snapshot before the cut");
    let times = snapshot_times(&after);
    assert!(
        times.len() > 100,
        "the replay and the live stream after it: {}",
        times.len()
    );
    for (i, t) in times.iter().enumerate() {
        assert_eq!(
            *t,
            last_seen + (i as u64 + 1) * STEP_NS,
            "snapshot {i} after the resume is not the next step"
        );
    }
    // And no resync keyframe was synthesised for it. Every keyframe this encoder writes
    // carries FLAG_RESYNC (a keyframe re-seeds interpolation, §2.3), so the flag cannot tell
    // a GOP boundary from a resync; the time can. A connection's GOPs start at its first
    // keyframe and fall every simulated second after it (§3.1.1's default period); a
    // keyframe anywhere else would be the resync.
    let origin = before
        .iter()
        .find(|(_, t, _, _)| *t == MsgType::Keyframe.id())
        .map(|(_, _, _, time)| *time)
        .expect("the stream opened with a keyframe");
    let off_cadence: Vec<u64> = after
        .iter()
        .filter(|(_, t, _, _)| *t == MsgType::Keyframe.id())
        .map(|(_, _, _, time)| *time)
        .filter(|time| (time - origin) % 1_000_000_000 != 0)
        .collect();
    assert!(
        off_cadence.is_empty(),
        "keyframes off the one-second cadence after the resume: {off_cadence:?}"
    );
    let _ = ws.close(None).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_the_ring_no_longer_holds_is_a_fresh_hello_and_a_resync() {
    let server = server(false).await;
    let proxy = Proxy::to(server.address()).await;
    let mut ws = open(proxy.addr, "").await;
    let first = hello(&mut ws).await;
    let _ = canonical(&mut ws, 10).await;
    proxy.cut().await;
    drop(ws);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A `seq` the session never produced is not in its ring.
    let mut ws = open(
        proxy.addr,
        &format!("&session={}&resume=99999999", first.token),
    )
    .await;
    let again = hello(&mut ws).await;
    assert_eq!(
        again.flags & HELLO_RESUMED,
        0,
        "not resumable, so not resumed"
    );
    let after = canonical(&mut ws, 1).await;
    assert_eq!(after[0].1, MsgType::Keyframe.id(), "a keyframe first");
    assert_ne!(after[0].2 & FLAG_RESYNC, 0, "with FLAG_RESYNC");
    assert_eq!(
        after[0].0, again.resume_seq,
        "at the seq the Hello announced"
    );
    let _ = ws.close(None).await;

    // No token at all: §1.4 rule 2 as well.
    let mut ws = open(proxy.addr, "&resume=5").await;
    let fresh = hello(&mut ws).await;
    assert_eq!(fresh.flags & HELLO_RESUMED, 0);
    assert_ne!(fresh.token, first.token, "a new session has a new token");
    let _ = ws.close(None).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_run_resumes_at_the_head_with_nothing_to_replay() {
    let server = server(true).await;
    let proxy = Proxy::to(server.address()).await;
    let mut ws = open(proxy.addr, "").await;
    let first = hello(&mut ws).await;
    // A paused run sends the state at its position — a resync keyframe and whatever
    // accompanies it — and then nothing.
    let mut last = None;
    while let Ok(Some(frame)) =
        tokio::time::timeout(Duration::from_millis(400), next_frame(&mut ws)).await
    {
        let h = frame.header().expect("header");
        if MsgType::from_id(h.msg_type).is_some_and(MsgType::is_canonical) {
            last = Some(h.seq);
        }
    }
    // Nothing canonical at all is possible (a fixture paused at t = 0 has no state yet);
    // the next seq is then the one the Hello announced.
    let next = last.map_or(first.resume_seq, |seq| seq + 1);
    proxy.cut().await;
    drop(ws);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut ws = open(
        proxy.addr,
        &format!("&session={}&resume={next}", first.token),
    )
    .await;
    let again = hello(&mut ws).await;
    assert_ne!(
        again.flags & HELLO_RESUMED,
        0,
        "a client that missed nothing resumes"
    );
    assert_eq!(again.resume_seq, next);
    // Nothing canonical until the run moves.
    let quiet = tokio::time::timeout(Duration::from_millis(400), next_frame(&mut ws)).await;
    assert!(
        quiet.is_err(),
        "a paused, resumed stream sends nothing: {quiet:?}"
    );
    let _ = ws.close(None).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_connection_with_the_token_supersedes_the_first() {
    let server = server(false).await;
    let mut a = open(server.address(), "").await;
    let first = hello(&mut a).await;
    let seen = canonical(&mut a, 20).await;
    let next = seen.last().expect("frames").0 + 1;

    let mut b = open(
        server.address(),
        &format!("&session={}&resume={next}", first.token),
    )
    .await;
    let again = hello(&mut b).await;
    assert_ne!(
        again.flags & HELLO_RESUMED,
        0,
        "the new socket resumes the session"
    );

    // The old socket's last word is Bye{reason = 4}, then close 1012.
    let mut bye_reason = None;
    let mut close_code = None;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_secs(5), a.next()).await {
        match msg {
            Message::Binary(bytes) => {
                let frame = Frame::from_bytes(bytes.to_vec()).expect("frame");
                if frame.header().expect("header").msg_type == MsgType::Bye.id() {
                    bye_reason = frame.body().get(16).copied();
                }
            }
            Message::Close(Some(CloseFrame { code, .. })) => {
                close_code = Some(u16::from(code));
                break;
            }
            Message::Close(None) => break,
            _ => {}
        }
    }
    assert_eq!(bye_reason, Some(4), "Bye{{reason = 4 (superseded)}}");
    assert_eq!(close_code, Some(1012), "close 1012");

    let after = canonical(&mut b, 60).await;
    assert_dense(&after, next);
    let _ = b.close(None).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clean_close_is_not_resumable() {
    let server = server(false).await;
    let mut ws = open(server.address(), "").await;
    let first = hello(&mut ws).await;
    let seen = canonical(&mut ws, 10).await;
    let next = seen.last().expect("frames").0 + 1;
    ws.close(Some(CloseFrame {
        code: 1000.into(),
        reason: "done".into(),
    }))
    .await
    .expect("close");
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await {}
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut ws = open(
        server.address(),
        &format!("&session={}&resume={next}", first.token),
    )
    .await;
    let again = hello(&mut ws).await;
    assert_eq!(
        again.flags & HELLO_RESUMED,
        0,
        "a client that said goodbye with 1000 is not holding a session"
    );
    let _ = ws.close(None).await;
    server.stop().await;
}

/// The symbol table a resumed `Hello` carries is the client's, at the resume point.
///
/// §2.5: ids are never reassigned within a connection, and a §3.8 extension's entries take
/// the ids after the table's current end. A resumed `Hello` that carried the table as it
/// is *now* would include the strings of a `Provenance` frame the client has not received;
/// the replay would then append them a second time and every id after them would resolve
/// one string off. This drives a session through exactly that: a `Hello`, then a step whose
/// `Provenance` extends the table, then a client that saw only the `Hello`.
#[test]
fn a_resumed_hello_carries_the_table_the_client_had_and_the_replay_extends_it() {
    use v2xw_record::wire::StrTable;
    use v2xw_record::wire::provenance::ProvenanceBody;
    use v2xw_server::engine::Control;
    use v2xw_server::{Run, Session, StubEngine};

    let engine = StubEngine::new(StubOptions {
        actors: 6,
        grid: 3,
        duration_s: 30,
        paused: true,
        ..StubOptions::default()
    })
    .expect("fixture");
    let world_json = v2xw_world::serde_vwp::to_json_string(engine.geometry()).expect("world");
    let run = Run::new(Box::new(engine), world_json).expect("run");
    let descriptor = run.descriptor();
    let mut session = Session::new(Default::default(), &descriptor);
    session.bind(run.generation(), "0123456789abcdef0123456789abcdef");
    let greeting = session.greet(&run, &descriptor).expect("greet");
    let first = HelloBody::decode(greeting[0].body()).expect("Hello");
    let mut client: StrTable = first.strings.clone();

    let mut rx = run.subscribe();
    run.control(Control::Resume).expect("resume");
    assert!(run.tick().expect("tick"));
    let step = rx.try_recv().expect("a step");
    let frames = session.encode_step(&step).expect("encode").frames;
    let extended = frames.iter().any(|f| {
        f.header().expect("header").msg_type == MsgType::Provenance.id()
            && ProvenanceBody::decode(f.body())
                .expect("provenance")
                .strings
                .is_some_and(|t| !t.strings.is_empty())
    });
    assert!(
        extended,
        "the first step carries a Provenance frame that extends the table"
    );

    // The client saw the Hello and nothing else, so it resumes at seq 0.
    session.reattach(Some(0));
    let again = session.greet(&run, &descriptor).expect("greet again");
    let resumed = HelloBody::decode(again[0].body()).expect("resumed Hello");
    assert_ne!(resumed.hello_flags & HELLO_RESUMED, 0);
    assert_eq!(
        resumed.strings.strings, client.strings,
        "the resumed Hello's table is the one the client holds at seq 0"
    );
    // The client applies the replay exactly as it would have applied the originals.
    let backlog = session.resume_backlog();
    assert_eq!(backlog.len(), frames.len(), "every missed frame, once");
    for frame in &backlog {
        if frame.header().expect("header").msg_type == MsgType::Provenance.id() {
            let body = ProvenanceBody::decode(frame.body()).expect("provenance");
            let base = client.strings.len() as u32;
            if let Some(ext) = &body.strings {
                client.strings.extend(ext.strings.iter().cloned());
            }
            // Every id the frame quotes resolves to a string the frame meant.
            for entry in &body.entries {
                let model = client.get(entry.str_model_id).expect("resolvable");
                assert!(entry.str_model_id >= base, "an extension id");
                assert!(
                    model.contains('/'),
                    "a model id, not an off-by-one: {model:?}"
                );
            }
        }
    }

    // A client that saw everything resumes at the head, and its table is the whole table.
    let head = session.next_seq();
    session.reattach(Some(head));
    let at_head = session.greet(&run, &descriptor).expect("greet at head");
    let body = HelloBody::decode(at_head[0].body()).expect("Hello at head");
    assert_ne!(body.hello_flags & HELLO_RESUMED, 0);
    assert_eq!(body.strings.strings, client.strings);
    assert!(
        session.resume_backlog().is_empty(),
        "nothing to replay at the head"
    );
}
