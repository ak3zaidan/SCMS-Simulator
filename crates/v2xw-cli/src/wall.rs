//! The only wall-clock reads in this workspace's engine path, and why they are here.
//!
//! 02-architecture.md §6.1 forbids a wall-clock read in engine-facing code: simulated time
//! comes from [`v2xw_core::SimTime`], and that is why [`v2xw_engine::Engine::build`] takes
//! its manifest timestamp as an argument and [`v2xw_world::ImportOptions`] takes its import
//! date as one. Somebody has to supply those, and that somebody is the operator's tool.
//!
//! Both uses below are confined to this module so that the rule can be checked by reading
//! one file:
//!
//! * [`now_iso8601_utc`] answers "when was this run started?" for
//!   [`v2xw_core::manifest::Manifest::build_utc`] and for an import date. It is the caller's
//!   timestamp, excluded from every digest, and `--build-utc` overrides it so that a
//!   reproducibility comparison can pin it.
//! * [`Stopwatch`] measures how long the tool took. It is a measurement *of* the run, never
//!   an input *to* it: no value it produces reaches the engine, the scenario, a record or
//!   the manifest's hashed fields.
//!
//! No other module in this crate may read a clock.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// The current instant as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Formatted here rather than through a date library because the workspace has no date
/// dependency and adding one to print a timestamp would be the wrong trade. The
/// civil-from-days conversion is Howard Hinnant's `civil_from_days`, integer-only, valid
/// for every date in the proleptic Gregorian calendar; leap seconds do not exist in Unix
/// time, so there is no table to get wrong.
///
/// A clock before the epoch (a machine with its date unset) yields
/// `"1970-01-01T00:00:00Z"` rather than an error: the field pins nothing on such a machine
/// either way, and refusing to run over it would be worse than recording it.
pub fn now_iso8601_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso8601_utc(secs)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a count of seconds since the Unix epoch.
///
/// Split out from [`now_iso8601_utc`] so it can be tested: a formatter that only ever sees
/// "now" is a formatter nobody has checked against a known date.
pub fn iso8601_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60,
    );
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days since 1970-01-01 to `(year, month, day)` — Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Wall-clock elapsed time, for reporting how long the tool took.
///
/// Never an input to a run: see the module documentation.
#[derive(Debug)]
pub struct Stopwatch(Instant);

impl Stopwatch {
    /// Starts one.
    pub fn start() -> Self {
        Stopwatch(Instant::now())
    }

    /// Seconds elapsed since [`Stopwatch::start`].
    pub fn elapsed_s(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }

    /// Milliseconds elapsed, rounded, for human-readable output.
    pub fn elapsed_ms(&self) -> u128 {
        self.0.elapsed().as_millis()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three dates with known answers, including a leap day and a pre-epoch instant.
    #[test]
    fn the_formatter_agrees_with_known_dates() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        // 2026-09-22T13:45:07Z, checked against `date -u -r 1790084707`.
        assert_eq!(iso8601_utc(1_790_084_707), "2026-09-22T13:45:07Z");
        // A leap day: 2024-02-29T23:59:59Z.
        assert_eq!(iso8601_utc(1_709_251_199), "2024-02-29T23:59:59Z");
        // Before the epoch: the arithmetic is euclidean, so it does not wrap.
        assert_eq!(iso8601_utc(-1), "1969-12-31T23:59:59Z");
    }
}
