//! `v2xw run` — run a scenario and write its recording, metrics and manifest.
//!
//! # What a run leaves behind
//!
//! | File | What it is | Reproducible? |
//! |---|---|---|
//! | `recording.mcap` | the MCAP container: every record, the manifest metadata, the scenario attachment | its **data section** is; its file digest is not, see below |
//! | `scenario.resolved.yaml` | the scenario after base merge, migration and defaults — the document the run actually executed | yes |
//! | `metrics.json` | every `metric.sample` record the run emitted | yes |
//! | `run-report.json` | event, frame and record counts, per channel, plus the recording's content digest | yes |
//! | `manifest.json` | [`v2xw_core::manifest::Manifest`] with the four files above digested and `data_digest` finalised | all but `build_utc` and the recording's entry |
//!
//! # Why the file digest of an MCAP is not the run's digest
//!
//! [`v2xw_record::Reader::content_digest`] documents it: the `mcap` 0.25 writer emits the
//! repeated schema and channel records of the *summary section* from a `HashMap`, so their
//! order — and with it the file's SHA-256 — can vary between two runs of the same program
//! on the same machine. The summary is an index; reordering an index changes no content.
//! So this command reports **both**: `manifest.json` carries the honest SHA-256 of the
//! bytes on disk, and `run-report.json` carries `content_digest`, which is the SHA-256 over
//! every message's topic, instant and bytes in stored order. Two runs of one scenario must
//! agree on the second. They are allowed to disagree on the first, and `v2xw run --json`
//! prints both so the operator can see which one moved.
//!
//! # No clock reaches the engine
//!
//! `--build-utc` is the manifest's caller-supplied timestamp; left out, it comes from
//! [`crate::wall::now_iso8601_utc`], the only clock read on this path. The stopwatch that
//! times the run is a measurement of the tool, never an input to it, and nothing it
//! produces is written to a digested field.

use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use v2xw_core::time::Duration;
use v2xw_engine::{Engine, MemoryRecorder, NullRecorder, RunReport, Scenario};
use v2xw_record::{Cadence, Profile, RecordingOptions, RecordingWriter};

use crate::error::{CliError, Result};
use crate::tally::{ChannelTally, Tally};
use crate::wall::{Stopwatch, now_iso8601_utc};

/// How `v2xw run` was asked to run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The scenario file.
    pub scenario: PathBuf,
    /// Where the outputs go. `None` means `runs/<meta.name>`.
    pub out: Option<PathBuf>,
    /// The manifest's caller-supplied timestamp. `None` reads the clock.
    pub build_utc: Option<String>,
    /// How often a keyframe is declared in the recording's cadence, milliseconds.
    pub keyframe_ms: u64,
    /// Record the `NODE-only` profile, which refuses every ground-truth record.
    pub node_only: bool,
    /// Write a recording at all. `false` runs the engine into a counting sink, which is
    /// what a scaling measurement wants: it measures the engine and not the container.
    pub record: bool,
    /// Also attach the world payload to the recording. Off by default: a city world is
    /// tens of megabytes and most runs do not need it carried twice.
    pub attach_world: bool,
    /// Read the recording back and verify it before reporting.
    pub verify: bool,
    /// Override `time.duration_s`. Changes the scenario hash, as it should.
    pub duration_s: Option<f64>,
    /// Override `actors.vehicles.demand.rate_veh_per_h`. Changes the scenario hash.
    pub rate_veh_per_h: Option<f64>,
    /// Print the outcome as JSON on stdout instead of as a table.
    pub json: bool,
}

/// What a run produced, in the shape `--json` prints and `run-report.json` stores.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunOutcome {
    /// The scenario's name.
    pub scenario_name: String,
    /// Where the outputs went.
    pub out_dir: String,
    /// The scenario hash the run executed under.
    pub scenario_hash: String,
    /// The world's content hash.
    pub world_hash: String,
    /// The master seed, as hex.
    pub master_seed_hex: String,
    /// Simulated seconds the scenario asked for.
    pub duration_s: f64,
    /// The engine's own run report.
    pub report: RunReport,
    /// Per-channel record counts.
    pub channels: BTreeMap<String, ChannelTally>,
    /// How many metric samples were emitted.
    pub metric_samples: u64,
    /// The recording's size in bytes, when one was written.
    pub recording_bytes: Option<u64>,
    /// SHA-256 of the recording file's bytes — *not* reproducible, see the module note.
    pub recording_file_sha256: Option<String>,
    /// SHA-256 over the recording's data section — the number two runs must agree on.
    pub content_digest: Option<String>,
    /// What the reader's verification found, when `--verify` was on.
    pub verified: Option<VerifiedSummary>,
    /// Container counters the writer reported.
    pub container: Option<ContainerSummary>,
    /// Timings, which are wall-clock measurements of the tool and reach no digest.
    pub timing: Timing,
    /// What the scenario's `exporters` wrote, over the recording (`v2xw_engine::export`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<v2xw_engine::export::Exported>,
}

/// What reading the recording back found.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifiedSummary {
    /// Records walked.
    pub records: u64,
    /// VWP frames walked.
    pub frames: u64,
    /// Chunks whose stored CRC was present and matched.
    pub chunks_checksummed: u64,
    /// Chunks that declared no CRC, so nothing in them was checked.
    pub chunks_without_checksum: u64,
    /// True only when every chunk carried a checksum and it matched.
    pub integrity_verified: bool,
}

/// What the container did with the bytes.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ContainerSummary {
    /// Messages written, both encodings together.
    pub message_count: u64,
    /// Chunks the writer closed.
    pub chunk_count: u64,
    /// The largest number of uncompressed message bytes in one chunk.
    pub largest_chunk_bytes: u64,
}

/// Wall-clock seconds, by phase. Never an input to anything.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Timing {
    /// Loading and merging the scenario.
    pub load_s: f64,
    /// [`Engine::build`], which is where the world import happens.
    pub build_s: f64,
    /// [`Engine::run`].
    pub run_s: f64,
    /// Reading the recording back.
    pub verify_s: f64,
    /// Everything, end to end.
    pub total_s: f64,
    /// Wall-clock seconds per simulated second of `run_s` alone.
    pub wall_s_per_sim_s: f64,
}

/// Runs a scenario.
///
/// # Errors
/// [`CliError::Engine`] if the scenario will not load, will not validate or the run fails;
/// [`CliError::Record`] if the recording cannot be written or read back;
/// [`CliError::Io`] for any output that cannot be written.
pub fn run(opts: &RunOptions) -> Result<RunOutcome> {
    let total = Stopwatch::start();

    let load = Stopwatch::start();
    let mut scenario = Scenario::load(&opts.scenario)?;
    apply_overrides(&mut scenario, opts);
    let load_s = load.elapsed_s();

    let name = scenario.meta.name.clone();
    let duration_s = scenario.time.duration_s;
    let mobility_step = scenario.time.mobility_step();
    let out_dir = opts
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("runs").join(&name));
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| CliError::io("cannot create output directory", &out_dir, e))?;

    let build_utc = opts
        .build_utc
        .clone()
        .unwrap_or_else(|| now_iso8601_utc().to_string());

    // Every exporter reads the recording, so a run that names one must record.
    let exporters = scenario.exporters.clone();
    if !exporters.is_empty() && !opts.record {
        return Err(v2xw_engine::EngineError::Scenario(v2xw_engine::ScenarioError::conflict(
            "exporters",
            "names exporters, and every exporter reads the recording, which --no-recording \
             turns off",
        ))
        .into());
    }

    let build = Stopwatch::start();
    let mut engine = Engine::build(scenario, &build_utc)?;
    let build_s = build.elapsed_s();

    // The resolved scenario is what the run executed, not what the author typed: bases are
    // merged, migrations applied and defaults filled. It is also what gets attached to the
    // recording, so an attachment and a hash can never describe different documents.
    let resolved = engine
        .scenario()
        .to_yaml()
        .map_err(v2xw_engine::EngineError::from)?;
    let manifest = engine.manifest().clone();

    let cadence = cadence_for(opts.keyframe_ms, mobility_step)?;
    let profile = if opts.node_only {
        Profile::NodeOnly
    } else {
        Profile::Full
    };

    let recording_path = out_dir.join("recording.mcap");
    let (report, channels, metric_samples, container) = if opts.record {
        let writer = RecordingWriter::create(
            &recording_path,
            RecordingOptions {
                cadence,
                profile,
                ..RecordingOptions::default()
            },
        )?;
        let mut tally = Tally::new(writer);
        // The manifest goes in before the first record, so that a run killed halfway still
        // says what produced the bytes it did write.
        tally
            .inner_mut()
            .write_manifest(&manifest.to_json_pretty()?)?;
        tally
            .inner_mut()
            .attach("scenario.yaml", "application/yaml", resolved.as_bytes())?;
        if opts.attach_world {
            let payload = v2xw_world::serde_vwp::write(engine.world())?;
            tally
                .inner_mut()
                .attach("world.vwb", "application/octet-stream", &payload.bytes)?;
        }
        let run = Stopwatch::start();
        let report = engine.run(&mut tally)?;
        let run_s = run.elapsed_s();
        let channels = tally.by_channel().clone();
        let samples = tally.metric_samples().to_vec();
        let summary = tally.into_inner().finish()?;
        (
            (report, run_s),
            channels,
            samples,
            Some(ContainerSummary {
                message_count: summary.message_count,
                chunk_count: summary.chunk_count,
                largest_chunk_bytes: summary.largest_chunk_bytes,
            }),
        )
    } else {
        let mut tally = Tally::new(NullRecorder::new());
        let run = Stopwatch::start();
        let report = engine.run(&mut tally)?;
        let run_s = run.elapsed_s();
        let channels = tally.by_channel().clone();
        let samples = tally.metric_samples().to_vec();
        ((report, run_s), channels, samples, None)
    };
    let (report, run_s) = report;

    let mut outcome = RunOutcome {
        scenario_name: name,
        out_dir: out_dir.display().to_string(),
        scenario_hash: manifest.scenario_hash.clone(),
        world_hash: manifest.world_hash.clone(),
        master_seed_hex: format!("{:#x}", manifest.master_seed),
        duration_s,
        report,
        channels,
        metric_samples: metric_samples.len() as u64,
        recording_bytes: None,
        recording_file_sha256: None,
        content_digest: None,
        verified: None,
        container,
        timing: Timing::default(),
        exports: Vec::new(),
    };

    let mut verify_s = 0.0;
    if opts.record {
        let bytes = std::fs::read(&recording_path)
            .map_err(|e| CliError::io("cannot read back recording", &recording_path, e))?;
        outcome.recording_bytes = Some(bytes.len() as u64);
        outcome.recording_file_sha256 = Some(v2xw_core::hash::sha256_hex(&bytes));
        if opts.verify {
            let v = Stopwatch::start();
            let mut reader = v2xw_record::Reader::open_bytes(bytes)?;
            let digest = reader.content_digest()?;
            outcome.content_digest = Some(v2xw_core::hash::hex_encode(&digest));
            let report = reader.verify()?;
            outcome.verified = Some(VerifiedSummary {
                records: report.records,
                frames: report.frames,
                chunks_checksummed: report.chunks_checksummed,
                chunks_without_checksum: report.chunks_without_checksum,
                integrity_verified: report.integrity_verified(),
            });
            verify_s = v.elapsed_s();
        }
    }

    if !exporters.is_empty() {
        outcome.exports = v2xw_engine::export::run_exporters(&exporters, &recording_path, &out_dir)?;
    }

    // The deterministic artefacts are written before the manifest, because the manifest
    // digests them.
    let mut manifest = manifest;
    write_and_digest(
        &mut manifest,
        &out_dir,
        "scenario.resolved.yaml",
        resolved.as_bytes(),
    )?;
    let metrics_json = serde_json::to_string_pretty(&serde_json::json!({
        "samples": metric_samples,
    }))
    .map_err(|e| CliError::Json {
        what: "metrics",
        source: e,
    })?;
    write_and_digest(
        &mut manifest,
        &out_dir,
        "metrics.json",
        metrics_json.as_bytes(),
    )?;

    // The report file deliberately excludes the timings: they are wall-clock numbers, and
    // a digested artefact that carries one is an artefact that never matches itself.
    // Three fields are dropped from the digested copy, each for the same reason: they are
    // facts about *this invocation* and not about the run. Leaving any of them in makes
    // `run-report.json` differ between two runs of one scenario, which would put a
    // spurious mismatch into the manifest's `data_digest` and make a real one unnoticeable.
    let mut deterministic = outcome.clone();
    deterministic.timing = Timing::default();
    deterministic.recording_file_sha256 = None;
    deterministic.out_dir = String::new();
    let report_json = serde_json::to_string_pretty(&deterministic).map_err(|e| CliError::Json {
        what: "run report",
        source: e,
    })?;
    write_and_digest(
        &mut manifest,
        &out_dir,
        "run-report.json",
        report_json.as_bytes(),
    )?;
    if let Some(sha) = &outcome.recording_file_sha256 {
        manifest.files.push(v2xw_core::manifest::FileDigest {
            path: "recording.mcap".to_string(),
            sha256: sha.clone(),
        });
    }
    manifest.finalize();
    let manifest_json = manifest.to_json_pretty()?;
    write_file(&out_dir, "manifest.json", manifest_json.as_bytes())?;

    outcome.timing = Timing {
        load_s,
        build_s,
        run_s,
        verify_s,
        total_s: total.elapsed_s(),
        wall_s_per_sim_s: if duration_s > 0.0 {
            run_s / duration_s
        } else {
            f64::NAN
        },
    };
    Ok(outcome)
}

/// Runs a scenario into memory and returns the record digest, for a determinism check that
/// does not go near a file.
///
/// This is what "two runs of one scenario produce identical outputs" means at its
/// narrowest: no container, no manifest, no timestamp — just the record stream.
///
/// # Errors
/// [`CliError::Engine`] if the scenario will not load or the run fails.
pub fn digest_only(scenario: &Path, build_utc: &str) -> Result<String> {
    let scenario = Scenario::load(scenario)?;
    let mut engine = Engine::build(scenario, build_utc)?;
    let mut recorder = MemoryRecorder::new();
    engine.run(&mut recorder)?;
    Ok(recorder.digest_hex())
}

/// The recording cadence: keyframes every `keyframe_ms`, one delta per mobility step.
fn cadence_for(keyframe_ms: u64, mobility_step: Duration) -> Result<Cadence> {
    Cadence::new(Duration::from_millis(keyframe_ms), mobility_step).map_err(|e| {
        CliError::BadArgument {
            flag: "keyframe-ms",
            problem: format!(
                "{keyframe_ms} ms is not a usable keyframe period against this scenario's \
                 {} ms mobility step: {e}",
                mobility_step.as_nanos() / 1_000_000
            ),
        }
    })
}

/// Applies the two scale overrides, which exist so that one scenario can be swept without
/// four near-identical copies of it on disk. Both change the scenario hash, because both
/// change the scenario the engine executes.
fn apply_overrides(scenario: &mut Scenario, opts: &RunOptions) {
    if let Some(d) = opts.duration_s {
        scenario.time.duration_s = d;
    }
    if let Some(r) = opts.rate_veh_per_h {
        scenario.actors.vehicles.demand.rate_veh_per_h = Some(r);
    }
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = dir.join(name);
    let mut f = BufWriter::new(
        std::fs::File::create(&path).map_err(|e| CliError::io("cannot write", &path, e))?,
    );
    f.write_all(bytes)
        .map_err(|e| CliError::io("cannot write", &path, e))?;
    f.flush()
        .map_err(|e| CliError::io("cannot write", &path, e))
}

fn write_and_digest(
    manifest: &mut v2xw_core::manifest::Manifest,
    dir: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    write_file(dir, name, bytes)?;
    manifest.add_file(name, bytes);
    Ok(())
}
