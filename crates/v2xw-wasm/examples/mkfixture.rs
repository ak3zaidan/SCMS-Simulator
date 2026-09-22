//! Writes a recording and the native reader's answers about it, for the Node smoke test.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p v2xw-wasm --example mkfixture -- target/wasm-fixture
//! ```
//!
//! It produces `replay.mcap` and `expected.json` in that directory. The JSON is the
//! **native** build's answer — every seek's keyframe time, resolved position, delta count
//! and every resolved pose column — so `tests/node/smoke.mjs` can hold the WebAssembly
//! build to it column by column rather than to a summary of it.
//!
//! The JSON is written by hand rather than through `serde`: it has one shape, this is the
//! only thing that writes it, and a hand-written writer keeps the example's dependency
//! list at exactly the crate it is demonstrating.

use std::fmt::Write as _;

use v2xw_record::encoder::Cadence;
use v2xw_record::fixture::{self, RunShape};
use v2xw_wasm::{ReplaySession, Scene};

/// The run both builds read. Small enough that the JSON stays readable, long enough to
/// span several GOPs and to contain the absolute-escape teleport of §3.2.
const SHAPE: RunShape = RunShape {
    actors: 64,
    steps: 300,
    signals: 6,
    cadence: Cadence::DEFAULT,
    profile: v2xw_record::Profile::Full,
    teleport_at: Some(97),
    parked_attacker: false,
};

/// How many seek targets to record, spread evenly over the run.
const TARGETS: u64 = 12;

/// The chunk target, well below the recorder's 4 MiB default (vwp-v1 §7.1).
///
/// Deliberately small so this fixture is many chunks while staying a small file: a
/// recording that fits in one chunk cannot demonstrate anything about range requests,
/// because one seek legitimately reads all of it.
const CHUNK_TARGET_BYTES: u64 = 32 * 1024;

fn column<T: std::fmt::Display>(out: &mut String, name: &str, values: &[T], last: bool) {
    let _ = write!(out, "      \"{name}\": [");
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{v}");
    }
    out.push(']');
    if !last {
        out.push(',');
    }
    out.push('\n');
}

fn columns(out: &mut String, scene: &Scene) {
    column(out, "actor_id", scene.actor_id(), false);
    column(out, "x_mm", scene.x_mm(), false);
    column(out, "y_mm", scene.y_mm(), false);
    column(out, "z_mm", scene.z_mm(), false);
    column(out, "lane_id", scene.lane_id(), false);
    column(out, "heading_brad", scene.heading_brad(), false);
    column(out, "speed_cq", scene.speed_cq(), false);
    column(out, "accel_cq", scene.accel_cq(), false);
    column(out, "class_idx", scene.class_idx(), false);
    column(out, "state", scene.state(), false);
    column(out, "verified_neighbors", scene.verified_neighbors(), false);
    column(out, "signal_id", scene.signal_id(), false);
    column(out, "signal_ttc_ds", scene.signal_ttc_ds(), false);
    column(out, "signal_phase", scene.signal_phase(), true);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/wasm-fixture".to_string());
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("replay.mcap");
    fixture::write_recording_with(
        &path,
        &SHAPE,
        v2xw_record::RecordingOptions {
            cadence: SHAPE.cadence,
            profile: SHAPE.profile,
            chunk_target_bytes: CHUNK_TARGET_BYTES,
            ..Default::default()
        },
        true,
    )?;
    let bytes = std::fs::read(&path)?;

    let mut session = ReplaySession::from_bytes(bytes.clone())?;
    let (start, end) = session
        .span()?
        .ok_or("the recording has no snapshot span")?;

    let mut json = String::new();
    json.push_str("{\n");
    let _ = writeln!(json, "  \"file\": \"replay.mcap\",");
    let _ = writeln!(json, "  \"bytes\": {},", bytes.len());
    let _ = writeln!(json, "  \"chunks\": {},", session.index()?.chunks.len());
    let _ = writeln!(json, "  \"span_start_ns\": {start},");
    let _ = writeln!(json, "  \"span_end_ns\": {end},");
    let cadence = session.cadence()?;
    let _ = writeln!(
        json,
        "  \"keyframe_period_ns\": {},",
        cadence.keyframe_period.as_nanos()
    );
    let _ = writeln!(
        json,
        "  \"mobility_step_ns\": {},",
        cadence.mobility_step.as_nanos()
    );
    json.push_str("  \"seeks\": [\n");
    for i in 0..TARGETS {
        let t = start + (end - start) * i / (TARGETS - 1);
        let report = session
            .seek(t)?
            .done()
            .ok_or("a resident recording never waits for bytes")?;
        json.push_str("    {\n");
        let _ = writeln!(json, "      \"t_ns\": {t},");
        let _ = writeln!(
            json,
            "      \"keyframe_time_ns\": {},",
            report.keyframe_time_ns
        );
        let _ = writeln!(json, "      \"position_ns\": {},", report.position_ns);
        let _ = writeln!(json, "      \"deltas\": {},", report.deltas);
        let _ = writeln!(json, "      \"chunks_read\": {},", report.chunks_read);
        let _ = writeln!(json, "      \"actor_count\": {},", report.actor_count);
        let _ = writeln!(
            json,
            "      \"signal_count\": {},",
            session.scene().signal_count()
        );
        let origin = session.scene().origin();
        let _ = writeln!(
            json,
            "      \"origin\": [{}, {}, {}],",
            origin[0], origin[1], origin[2]
        );
        let _ = writeln!(
            json,
            "      \"keyframe_frame_len\": {},",
            session.keyframe_frame().map_or(0, <[u8]>::len)
        );
        columns(&mut json, session.scene());
        json.push_str(if i + 1 == TARGETS {
            "    }\n"
        } else {
            "    },\n"
        });
    }
    json.push_str("  ]\n}\n");

    let out = dir.join("expected.json");
    std::fs::write(&out, json)?;
    println!("wrote {} and {}", path.display(), out.display());
    Ok(())
}
