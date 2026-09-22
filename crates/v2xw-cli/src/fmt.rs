//! Rendering an outcome for a person.
//!
//! Separate from the commands so that `--json` and the human form print the *same* struct:
//! a tool whose table is computed on one path and whose JSON on another eventually
//! disagrees with itself, and the disagreement is always found by the person who trusted
//! the table.
//!
//! Numbers are printed with their units and never rounded away. A count of zero is printed
//! rather than omitted, because "no frames were transmitted" is the single most important
//! thing a first run can tell an author and a blank line does not say it.

use std::fmt::Write as _;

use crate::import::ImportOutcome;
use crate::info::InfoOutcome;
use crate::run::RunOutcome;
use crate::validate::ValidateOutcome;

/// Renders a run.
pub fn run(o: &RunOutcome) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "scenario        {}", o.scenario_name);
    let _ = writeln!(s, "seed            {}", o.master_seed_hex);
    let _ = writeln!(s, "scenario hash   {}", o.scenario_hash);
    let _ = writeln!(s, "world hash      {}", o.world_hash);
    let _ = writeln!(s, "outputs         {}", o.out_dir);
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "simulated       {:.3} s in {:.3} s wall ({:.3} wall s per simulated s)",
        o.duration_s, o.timing.run_s, o.timing.wall_s_per_sim_s
    );
    let _ = writeln!(
        s,
        "  load {:.3} s | build {:.3} s | run {:.3} s | verify {:.3} s | total {:.3} s",
        o.timing.load_s, o.timing.build_s, o.timing.run_s, o.timing.verify_s, o.timing.total_s
    );
    let _ = writeln!(s);
    let r = &o.report;
    let events: u64 = r.events_by_class.values().sum();
    let _ = writeln!(s, "events          {events}");
    for (class, n) in &r.events_by_class {
        let _ = writeln!(s, "  {class:<14}{n}");
    }
    let _ = writeln!(s, "mobility steps  {}", r.mobility_steps);
    let _ = writeln!(s, "actors spawned  {}", r.actors_spawned);
    let _ = writeln!(s, "nodes created   {}", r.nodes_created);
    let _ = writeln!(s, "frames sent     {}", r.frames_transmitted);
    let _ = writeln!(
        s,
        "receptions      {} attempted, {} received",
        r.reception_attempts, r.frames_received
    );
    let _ = writeln!(s, "suppressed      {}", r.suppressed_frames);
    let _ = writeln!(
        s,
        "records         {} written, {} refused",
        r.records, r.records_refused
    );
    let _ = writeln!(s);
    if o.channels.is_empty() {
        let _ = writeln!(s, "channels        none — this run recorded nothing");
    } else {
        let _ = writeln!(s, "channels");
        for (name, t) in &o.channels {
            let _ = writeln!(
                s,
                "  {name:<18}{:>8} records {:>10} json bytes",
                t.records, t.json_bytes
            );
        }
    }
    let _ = writeln!(s, "metric samples  {}", o.metric_samples);
    if let Some(c) = &o.container {
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "container       {} messages in {} chunk(s), largest chunk {} uncompressed bytes",
            c.message_count, c.chunk_count, c.largest_chunk_bytes
        );
    }
    if let Some(b) = o.recording_bytes {
        let _ = writeln!(s, "recording       {b} bytes on disk");
    }
    if let Some(d) = &o.content_digest {
        let _ = writeln!(s, "content digest  {d}");
    }
    if let Some(sha) = &o.recording_file_sha256 {
        let _ = writeln!(
            s,
            "file sha256     {sha}  (the container's summary section is not reproducible; \
             compare the content digest)"
        );
    }
    if let Some(v) = &o.verified {
        let _ = writeln!(
            s,
            "verified        {} records, {} frames; {}",
            v.records,
            v.frames,
            if v.integrity_verified {
                format!("integrity VERIFIED over {} chunk(s)", v.chunks_checksummed)
            } else {
                format!(
                    "integrity NOT VERIFIED: {} chunk(s) carried no checksum",
                    v.chunks_without_checksum
                )
            }
        );
    }
    s
}

/// Renders a validation.
pub fn validate(o: &ValidateOutcome) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "ok              {}", o.name);
    let _ = writeln!(s, "schema          {}", o.schema);
    let _ = writeln!(s, "scenario hash   {}", o.scenario_hash);
    let _ = writeln!(s, "seed            {}", o.master_seed_hex);
    let _ = writeln!(s, "world source    {}", o.world_source);
    let _ = writeln!(
        s,
        "time            {:.3} s at {} ms steps",
        o.duration_s, o.mobility_step_ms
    );
    let _ = writeln!(s, "metrics         {}", o.metrics.join(", "));
    for n in &o.notes {
        let _ = writeln!(s, "note            {n}");
    }
    s
}

/// Renders an import.
pub fn import(o: &ImportOutcome, report: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "speed preset    {} ({})",
        o.speed_preset, o.speed_preset_source
    );
    let _ = writeln!(
        s,
        "bbox            {}",
        o.bbox
            .as_deref()
            .unwrap_or("none — the frame comes from the extract's own bounds")
    );
    let _ = writeln!(s, "world hash      {}", o.world_hash);
    let _ = writeln!(s, "outputs         {}", o.out_dir);
    let _ = writeln!(
        s,
        "sizes           world.vwb {} bytes, world.v2xw {} bytes",
        o.payload_bytes, o.native_bytes
    );
    let _ = writeln!(s, "anomalies       {}", o.anomalies);
    for w in &o.precision_warnings {
        let _ = writeln!(s, "precision       {w}");
    }
    let _ = writeln!(s, "import took     {:.3} s", o.elapsed_s);
    let _ = writeln!(s);
    s.push_str(report);
    s
}

/// Renders a recording's contents.
pub fn info(o: &InfoOutcome) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "recording       {}", o.path);
    let _ = writeln!(s, "size            {} bytes", o.file_bytes);
    let _ = writeln!(s, "profile         {}", o.profile);
    let _ = writeln!(
        s,
        "cadence         keyframe {} ms, mobility step {} ms",
        o.keyframe_period_ns / 1_000_000,
        o.mobility_step_ns / 1_000_000
    );
    let _ = writeln!(s, "content digest  {}", o.content_digest);
    let _ = writeln!(s);
    let _ = writeln!(s, "channels");
    for c in &o.channels {
        let _ = writeln!(
            s,
            "  {:<26}{:<6}{:<8}{:>8} messages",
            c.topic, c.encoding, c.visibility, c.messages
        );
    }
    let _ = writeln!(s);
    if o.attachments.is_empty() {
        let _ = writeln!(s, "attachments     none");
    } else {
        let _ = writeln!(s, "attachments");
        for (name, len) in &o.attachments {
            let _ = writeln!(s, "  {name:<26}{len} bytes");
        }
    }
    let _ = writeln!(s);
    let v = &o.verify;
    let _ = writeln!(
        s,
        "verify          {} frames ({} keyframes, {} deltas, {} unknown), {} records",
        v.frames, v.keyframes, v.deltas, v.unknown_frames, v.records
    );
    let _ = writeln!(
        s,
        "integrity       {}",
        if v.integrity_verified {
            format!(
                "VERIFIED — {} chunk(s) checksummed and matched",
                v.chunks_checksummed
            )
        } else {
            format!(
                "NOT VERIFIED — {} chunk(s) matched, {} carried no checksum",
                v.chunks_checksummed, v.chunks_without_checksum
            )
        }
    );
    let _ = writeln!(s);
    match &o.manifest {
        Some(m) => {
            let _ = writeln!(s, "manifest");
            let text = serde_json::to_string_pretty(m).unwrap_or_else(|e| e.to_string());
            for line in text.lines() {
                let _ = writeln!(s, "  {line}");
            }
        }
        None => {
            let _ = writeln!(s, "manifest        none stored");
        }
    }
    if !o.metadata.is_empty() {
        let _ = writeln!(s, "metadata");
        for (k, v) in &o.metadata {
            let _ = writeln!(s, "  {k:<26}{v}");
        }
    }
    s
}

/// Renders a sweep.
///
/// Three things a person wants after a sweep, in the order they want them: did every run
/// finish, where are the files, and what did the table say. The per-metric block prints
/// the interval and the replication count together, because an estimate without them is
/// what this project's measurement layer exists to stop being printed.
pub fn experiment_run(o: &crate::experiment::ExperimentRunOutcome) -> String {
    let r = &o.outcome;
    let mut s = String::new();
    let _ = writeln!(s, "experiment      {}", r.name);
    let _ = writeln!(s, "plan            {}", r.plan_digest);
    let _ = writeln!(s, "outputs         {}", r.out_dir);
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "runs            {} planned, {} already done, {} run now, {} outstanding",
        r.runs_planned, r.runs_skipped, r.runs_executed, r.runs_pending
    );
    let _ = writeln!(
        s,
        "cells           {} sweep point(s), {} at a time",
        r.cells, r.concurrency
    );
    if r.concurrency > 1 {
        let _ = writeln!(
            s,
            "                running {} simulations at once holds {} worlds in memory at \
             the same time",
            r.concurrency, r.concurrency
        );
    }
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "results         {} row(s), {} with an estimate",
        r.rows, o.rows_with_estimate
    );
    let _ = writeln!(s, "results digest  {}", o.results_digest);
    if !o.exported.is_empty() {
        let _ = writeln!(s, "exported");
        for file in &o.exported {
            let _ = writeln!(s, "  {file}");
        }
    }
    let _ = writeln!(s, "files");
    for file in &r.files {
        let _ = writeln!(s, "  {file}");
    }
    if r.runs_pending > 0 {
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "{} run(s) outstanding — `v2xw experiment resume` continues from the journal",
            r.runs_pending
        );
    }
    s
}

/// Renders where a sweep stands.
pub fn experiment_status(o: &v2xw_experiment::ExperimentStatus) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "experiment      {}", o.name);
    let _ = writeln!(s, "plan            {}", o.plan_digest);
    let _ = writeln!(s, "outputs         {}", o.out_dir);
    let _ = writeln!(
        s,
        "sweep           {} cell(s) x {} seed slot(s) x {} replication(s)",
        o.cells, o.seed_slots, o.replications
    );
    let _ = writeln!(s);
    match &o.journal_plan_digest {
        None => {
            let _ = writeln!(
                s,
                "journal         none — this sweep has not been started here"
            );
        }
        Some(digest) => {
            let _ = writeln!(s, "journal         {digest}");
        }
    }
    if o.plan_changed {
        let _ = writeln!(
            s,
            "                the scenario's sweep has CHANGED since this directory was \
             started; a resume would mix two experiments and will be refused"
        );
    }
    let _ = writeln!(
        s,
        "progress        {} of {} run(s) done, {} outstanding",
        o.runs_completed, o.runs_planned, o.runs_pending
    );
    if !o.next_run_ids.is_empty() {
        let _ = writeln!(s, "next");
        for id in &o.next_run_ids {
            let _ = writeln!(s, "  {id}");
        }
        if o.runs_pending > o.next_run_ids.len() {
            let _ = writeln!(s, "  … and {} more", o.runs_pending - o.next_run_ids.len());
        }
    }
    s
}
