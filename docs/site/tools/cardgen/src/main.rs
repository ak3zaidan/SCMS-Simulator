//! Serialises the engine's model registry for the documentation site.
//!
//! The documentation site's model reference, calibration page and validation page are
//! generated from model cards rather than written by hand (ADR 0007 decision 3). The
//! cards do not exist as files: they are built in Rust, often from the same parameter
//! struct the model computes with, so a card's defaults are the defaults. The only
//! honest way to publish them is therefore to build the registry and serialise it,
//! which is all this program does.
//!
//! ```text
//! cargo run --release --manifest-path docs/site/tools/cardgen/Cargo.toml -- \
//!     --out docs/site/generated/cards.json
//! ```
//!
//! # Coverage, stated rather than implied
//!
//! Registration happens through crate-level entry points. Four exist today
//! (`v2xw-engine`'s wiring, which covers node and mobility, plus `v2xw-sec`,
//! `v2xw-proto` and `v2xw-metrics`). The radio, message, network and threat crates
//! publish their cards through per-model constructors instead, so their models are not
//! in this dump yet. That gap is written into the output as `coverage.uncovered` and
//! the site prints it, because a reference page that silently lists a subset is worse
//! than one that says which subset it is.
//!
//! # Failure is data
//!
//! A registration stage that fails does not abort the dump. It is recorded in
//! `stages[]` with its error, the remaining stages run, and the site shows the failure
//! on the model reference. A documentation build that dies because one crate's card
//! stopped validating tells the reader nothing; one that publishes "this stage failed,
//! here is why" tells them exactly what is missing.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use v2xw_core::registry::Registry;

/// The dump format's own version, bumped when the shape changes.
const DUMP_SCHEMA: &str = "v2xw/cards/1";

/// Crates whose models this exporter does not reach yet (see the module docs).
const UNCOVERED: &[&str] = &[
    "v2xw-radio",
    "v2xw-msg",
    "v2xw-net",
    "v2xw-threat",
    "v2xw-world",
    "v2xw-record",
];

/// Crates whose registrations this exporter drives.
const COVERED: &[&str] = &[
    "v2xw-node",
    "v2xw-mobility",
    "v2xw-sec",
    "v2xw-proto",
    "v2xw-metrics",
];

const HELP: &str = "\
v2xw-cardgen — serialise the engine's model registry for the documentation site

USAGE:
    v2xw-cardgen [--out <PATH>]

OPTIONS:
    --out <PATH>    where to write the dump [default: docs/site/generated/cards.json]
    -h, --help      print this message
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut out = PathBuf::from("docs/site/generated/cards.json");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => {
                let value = args.next().ok_or("--out needs a path")?;
                out = PathBuf::from(value);
            }
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    let mut registry = Registry::new();
    let mut stages: Vec<Value> = Vec::new();

    let before = registry.len();
    let result = v2xw_engine::wiring::register_all(&mut registry);
    stages.push(stage(
        "v2xw_engine::wiring::register_all",
        "node and mobility models",
        registry.len() - before,
        result.err().map(|e| e.to_string()),
    ));

    let before = registry.len();
    // `WallClock::new(0)` is the Unix epoch. The envelopes take a wall clock so that a
    // certificate's validity can be compared against simulated time; nothing in a card
    // depends on which instant it is, and passing a fixed one keeps the dump
    // reproducible. Reading the real clock here would make two dumps of one tree
    // differ, which is the property this repository spends most of its effort keeping.
    let wall = v2xw_core::time::WallClock::new(0);
    let result = v2xw_sec::register_all(&mut registry, wall);
    stages.push(stage(
        "v2xw_sec::register_all",
        "envelopes, primitives and crypto backends",
        registry.len() - before,
        result.err().map(|e| e.to_string()),
    ));

    let before = registry.len();
    let result = v2xw_proto::register_all(&mut registry);
    stages.push(stage(
        "v2xw_proto::register_all",
        "credential-management protocols",
        registry.len() - before,
        result.err().map(|e| e.to_string()),
    ));

    let before = registry.len();
    let mut providers = v2xw_metrics::ProviderSet::new();
    let result = v2xw_metrics::register_all(&mut registry, &mut providers, 0);
    stages.push(stage(
        "v2xw_metrics::register_all",
        "metric providers",
        registry.len() - before,
        result.err().map(|e| e.to_string()),
    ));

    let mut models: Vec<Value> = Vec::new();
    for (_model_ref, registered) in registry.iter_by_id() {
        let mut entry = Map::new();
        entry.insert("id".to_string(), Value::String(registered.card.id.clone()));
        entry.insert("licence".to_string(), serde_json::to_value(&registered.licence)?);
        entry.insert("hosting".to_string(), serde_json::to_value(&registered.hosting)?);
        entry.insert(
            "content_hash".to_string(),
            Value::String(registered.content_hash_hex()),
        );
        entry.insert("card".to_string(), serde_json::to_value(&registered.card)?);
        models.push(Value::Object(entry));
    }

    let mut coverage = Map::new();
    coverage.insert(
        "covered".to_string(),
        Value::Array(COVERED.iter().map(|c| Value::String((*c).to_string())).collect()),
    );
    coverage.insert(
        "uncovered".to_string(),
        Value::Array(UNCOVERED.iter().map(|c| Value::String((*c).to_string())).collect()),
    );

    let mut root = Map::new();
    root.insert("schema".to_string(), Value::String(DUMP_SCHEMA.to_string()));
    root.insert(
        "cardgen_version".to_string(),
        Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    root.insert(
        "engine_version".to_string(),
        Value::String(v2xw_engine::manifest::ENGINE_VERSION.to_string()),
    );
    root.insert("stages".to_string(), Value::Array(stages));
    root.insert("coverage".to_string(), Value::Object(coverage));
    root.insert("model_count".to_string(), Value::from(models.len()));
    root.insert("models".to_string(), Value::Array(models));

    write(&out, &Value::Object(root))?;
    println!(">> wrote {} models to {}", registry.len(), out.display());
    Ok(())
}

/// One registration stage's outcome, as the site reads it.
fn stage(name: &str, what: &str, registered: usize, error: Option<String>) -> Value {
    let mut map = Map::new();
    map.insert("name".to_string(), Value::String(name.to_string()));
    map.insert("provides".to_string(), Value::String(what.to_string()));
    map.insert("registered".to_string(), Value::from(registered));
    map.insert("ok".to_string(), Value::Bool(error.is_none()));
    map.insert(
        "error".to_string(),
        match error {
            Some(message) => Value::String(message),
            None => Value::Null,
        },
    );
    Value::Object(map)
}

/// Writes the dump, creating the parent directory if it is missing.
fn write(path: &Path, value: &Value) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    std::fs::write(path, text)?;
    Ok(())
}
