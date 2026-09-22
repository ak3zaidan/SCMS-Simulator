//! `v2xw` — the command-line control surface (ADR 0010).
//!
//! The binary is thin on purpose: it parses arguments, calls a library function, prints
//! the outcome and picks an exit code. Everything it can do is available to a test as a
//! plain function call, which is why `tests/` never shells out to the binary.
//!
//! Exit codes: `0` success, `1` the command failed, `2` the arguments were wrong (clap's
//! own code). A failure prints to stderr with the field or path it is about, never a bare
//! "error".

use std::process::ExitCode;

use clap::Parser;
use v2xw_cli::cli::{Cli, Command, ExperimentCommand};
use v2xw_cli::{experiment, fmt, import, info, run, validate};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("v2xw: {e}");
            // A scenario error knows which key it is about; saying so is the difference
            // between an error an author can act on and one they have to bisect
            // (03-interfaces.md §13).
            if let v2xw_cli::CliError::Engine(v2xw_engine::EngineError::Scenario(s)) = &e
                && let Some(field) = s.field()
            {
                eprintln!("v2xw: the offending key is `{field}`");
            }
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                eprintln!("  caused by: {s}");
                source = s.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: &Cli) -> v2xw_cli::Result<String> {
    match &cli.command {
        Command::Run(args) => {
            let outcome = run::run(&args.to_options())?;
            if args.json {
                json(&outcome, "run outcome")
            } else {
                Ok(fmt::run(&outcome))
            }
        }
        Command::Validate(args) => {
            let outcome = validate::validate(&args.scenario)?;
            if args.json {
                json(&outcome, "validation outcome")
            } else {
                Ok(fmt::validate(&outcome))
            }
        }
        Command::ImportOsm(args) => {
            let (outcome, report) = import::import_osm_extract(&import::ImportOptions {
                extract: args.extract.clone(),
                out: args.out.clone(),
                imported_at: args.imported_at.clone(),
                speed_preset: args.speed_preset.clone(),
                bbox: args.bbox.clone(),
            })?;
            if args.json {
                json(&outcome, "import outcome")
            } else {
                Ok(fmt::import(&outcome, &report))
            }
        }
        Command::Info(args) => {
            let outcome = info::info(&args.recording)?;
            if args.json {
                json(&outcome, "recording info")
            } else {
                Ok(fmt::info(&outcome))
            }
        }
        Command::Experiment(args) => match &args.command {
            // `run` and `resume` are one code path with one flag between them, so there is
            // no second implementation to drift.
            ExperimentCommand::Run(a) | ExperimentCommand::Resume(a) => {
                let resume = matches!(&args.command, ExperimentCommand::Resume(_));
                let outcome = experiment::run(&a.to_options(resume))?;
                if a.json {
                    json(&outcome, "experiment outcome")
                } else {
                    Ok(fmt::experiment_run(&outcome))
                }
            }
            ExperimentCommand::Status(a) => {
                let outcome = experiment::status(&a.scenario, a.out.as_deref())?;
                if a.json {
                    json(&outcome, "experiment status")
                } else {
                    Ok(fmt::experiment_status(&outcome))
                }
            }
        },
    }
}

fn json<T: serde::Serialize>(value: &T, what: &'static str) -> v2xw_cli::Result<String> {
    let mut s = serde_json::to_string_pretty(value)
        .map_err(|source| v2xw_cli::CliError::Json { what, source })?;
    s.push('\n');
    Ok(s)
}
