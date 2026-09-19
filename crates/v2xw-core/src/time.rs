//! Simulated time, the scenario wall clock, and IEEE 1609.2 time values.
//!
//! Simulated time is [`SimTime`]: `u64` nanoseconds since the scenario start `t0`
//! (03-interfaces.md §1). `u64` nanoseconds give 584 years of range; models are
//! guaranteed 1 µs resolution, and the extra precision lets symbol boundaries (8 µs),
//! slot times (13 µs) and propagation delays (1 µs per 300 m) be represented exactly
//! without float accumulation (ADR 0004 §1).
//!
//! Wall-clock time appears in exactly two places in the engine: the scenario's `t0`
//! (a civil datetime chosen by the scenario author, modelled here by [`WallClock`]) and
//! the caller-supplied [`crate::manifest::Manifest::build_utc`] string. Nothing in the
//! engine ever reads the host clock; [`WallClock`] is a pure function of the scenario.

use serde::{Deserialize, Serialize};

/// Nanoseconds since scenario start `t0`.
///
/// `u64` gives 584 years of range; the engine guarantees 1 µs resolution to models
/// (03-interfaces.md §1, ADR 0004 §1).
pub type SimTime = u64;

/// Nanoseconds in one microsecond.
pub const NS_PER_US: SimTime = 1_000;
/// Nanoseconds in one millisecond.
pub const NS_PER_MS: SimTime = 1_000_000;
/// Nanoseconds in one second.
pub const NS_PER_S: SimTime = 1_000_000_000;

/// A **span** of simulated time in nanoseconds, as distinct from the **instant**
/// [`SimTime`].
///
/// [`SimTime`] is an alias for `u64`, so the two are the same machine word and the type
/// system cannot tell "at 3 seconds" from "in 3 seconds". Models overwhelmingly mean the
/// second one: a MAC backoff, a CAM period, a verification service time, a DENM repetition
/// interval and a protocol timeout are all *delays*, and every one of them has to be turned
/// into an absolute deadline by adding it to `ctx.now()`. Writing that addition out by hand
/// in every model is where "scheduled in the past" panics come from. This newtype names the
/// span, and [`crate::event::Scheduler::schedule_after`] and [`crate::ctx::Ctx::schedule_after`]
/// do the addition once, correctly.
///
/// It is deliberately *not* [`std::time::Duration`]: that type measures wall-clock time,
/// which nothing in this engine may read (ADR 0004), and its nanosecond accessors are
/// `u128`. This one is exactly the unit the kernel counts in, so a conversion is never
/// needed on a hot path.
///
/// Arithmetic saturates rather than wrapping or panicking, in both profiles: `u64`
/// nanoseconds span 584 years, so an overflow can only come from a model that computed a
/// nonsense delay, and clamping to the end of time surfaces that as an event that never
/// fires rather than as a wrap-around that fires immediately in a release build.
/// Subtraction below zero clamps to [`Duration::ZERO`], because a negative span of
/// simulated time does not exist.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
#[repr(transparent)]
pub struct Duration(
    /// The span, nanoseconds.
    pub u64,
);

impl Duration {
    /// A zero-length span: "now", when used as a delay.
    pub const ZERO: Duration = Duration(0);

    /// The longest representable span, about 584 years. What a saturating overflow yields.
    pub const MAX: Duration = Duration(u64::MAX);

    /// A span of `ns` nanoseconds.
    pub const fn from_nanos(ns: u64) -> Self {
        Duration(ns)
    }

    /// A span of `us` microseconds, saturating at [`Duration::MAX`].
    pub const fn from_micros(us: u64) -> Self {
        Duration(us.saturating_mul(NS_PER_US))
    }

    /// A span of `ms` milliseconds, saturating at [`Duration::MAX`].
    pub const fn from_millis(ms: u64) -> Self {
        Duration(ms.saturating_mul(NS_PER_MS))
    }

    /// A span of `s` seconds, saturating at [`Duration::MAX`].
    pub const fn from_secs(s: u64) -> Self {
        Duration(s.saturating_mul(NS_PER_S))
    }

    /// A span of `seconds`, rounded to the nearest nanosecond by [`secs_to_ns`].
    ///
    /// Negative and `NaN` inputs give [`Duration::ZERO`], for the same reason
    /// [`secs_to_ns`] clamps: a model that computed a negative delay has a bug the kernel
    /// must not amplify into a wrap-around.
    pub fn from_secs_f64(seconds: f64) -> Self {
        Duration(secs_to_ns(seconds))
    }

    /// The span in nanoseconds.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// The span in seconds, as `f64`. For model inputs and display only; the kernel keeps
    /// time in integers.
    pub fn as_secs_f64(self) -> f64 {
        ns_to_secs(self.0)
    }

    /// True if the span is zero.
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// The instant `self` after `t`, saturating at the end of time.
    ///
    /// The spelling to use when the instant reads better than the delay:
    /// `deadline = timeout.after(ctx.now())`.
    pub const fn after(self, t: SimTime) -> SimTime {
        t.saturating_add(self.0)
    }

    /// The span from `from` to `to`, or [`Duration::ZERO`] if `to` is not after `from`.
    pub const fn between(from: SimTime, to: SimTime) -> Duration {
        Duration(to.saturating_sub(from))
    }

    /// This span multiplied by `k`, saturating.
    pub const fn saturating_mul(self, k: u64) -> Duration {
        Duration(self.0.saturating_mul(k))
    }

    /// This span divided by `k`, truncating — `None` when `k` is zero.
    ///
    /// The `checked_` prefix means in this crate what it means everywhere else in Rust
    /// ([`u64::checked_div`]): the operation that cannot be performed returns `None` rather
    /// than panicking. It used to be `Duration(self.0 / k)`, which aborted the run on
    /// `k == 0` — and, being a `const fn`, was a hard compile error in a const context. A
    /// model computing `period.checked_div(n_slots)` with an empty slot set is a plausible
    /// bug in a plug-in; killing the process for it is not a plausible response.
    ///
    /// Use [`Duration::saturating_div`] where a value is wanted unconditionally.
    ///
    /// ```
    /// use v2xw_core::time::Duration;
    /// assert_eq!(
    ///     Duration::from_millis(100).checked_div(4),
    ///     Some(Duration::from_millis(25))
    /// );
    /// assert_eq!(Duration::from_millis(100).checked_div(0), None);
    /// ```
    pub const fn checked_div(self, k: u64) -> Option<Duration> {
        match self.0.checked_div(k) {
            Some(ns) => Some(Duration(ns)),
            None => None,
        }
    }

    /// This span divided by `k`, truncating, saturating at [`Duration::MAX`] for `k == 0`.
    ///
    /// The total spelling, for the caller who wants a number and not a decision. Dividing
    /// by nothing has no finite answer, and `Duration::MAX` is the saturating reading of it
    /// — the same direction [`Duration::saturating_mul`] and [`core::ops::Add`] saturate
    /// in. It is an obviously wrong span rather than a plausible one, so a caller who
    /// reached it by accident sees it in the first frame rather than in a metric three
    /// phases later.
    ///
    /// ```
    /// use v2xw_core::time::Duration;
    /// assert_eq!(
    ///     Duration::from_millis(100).saturating_div(4),
    ///     Duration::from_millis(25)
    /// );
    /// assert_eq!(Duration::from_millis(100).saturating_div(0), Duration::MAX);
    /// ```
    pub const fn saturating_div(self, k: u64) -> Duration {
        match self.0.checked_div(k) {
            Some(ns) => Duration(ns),
            None => Duration::MAX,
        }
    }
}

impl core::ops::Add for Duration {
    type Output = Duration;
    /// Saturating sum of two spans.
    fn add(self, other: Duration) -> Duration {
        Duration(self.0.saturating_add(other.0))
    }
}

impl core::ops::Sub for Duration {
    type Output = Duration;
    /// Difference of two spans, clamped at [`Duration::ZERO`].
    fn sub(self, other: Duration) -> Duration {
        Duration(self.0.saturating_sub(other.0))
    }
}

impl core::ops::Mul<u64> for Duration {
    type Output = Duration;
    /// Saturating repetition of a span: `period * 10`.
    fn mul(self, k: u64) -> Duration {
        self.saturating_mul(k)
    }
}

impl core::ops::AddAssign for Duration {
    fn add_assign(&mut self, other: Duration) {
        *self = *self + other;
    }
}

impl core::ops::SubAssign for Duration {
    fn sub_assign(&mut self, other: Duration) {
        *self = *self - other;
    }
}

impl core::ops::Add<Duration> for u64 {
    type Output = SimTime;
    /// The instant `d` after this one, saturating: `ctx.now() + Duration::from_millis(100)`.
    ///
    /// Defined on `u64` because [`SimTime`] is an alias for it. Nothing else in the
    /// workspace may add a `Duration` to a `u64`, so this impl is unambiguous.
    fn add(self, d: Duration) -> SimTime {
        self.saturating_add(d.0)
    }
}

impl core::ops::Sub<Duration> for u64 {
    type Output = SimTime;
    /// The instant `d` *before* this one, clamped at `0`: simulated time has no negative
    /// part, so a delay longer than the elapsed run yields `t0`.
    fn sub(self, d: Duration) -> SimTime {
        self.saturating_sub(d.0)
    }
}

impl From<Duration> for u64 {
    fn from(d: Duration) -> u64 {
        d.0
    }
}

impl core::fmt::Display for Duration {
    /// Formats as seconds with nine decimals and an `s` suffix, e.g. `0.100000000s`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{:09}s", self.0 / NS_PER_S, self.0 % NS_PER_S)
    }
}

/// Unix timestamp of the IEEE 1609.2 epoch, 2004-01-01T00:00:00Z.
///
/// IEEE 1609.2 `Time32` counts seconds and `Time64` counts microseconds since this
/// epoch. The engine does not model leap seconds: it treats the 1609.2 time scale as
/// UTC-without-leap-seconds, which is what every simulator and most implementations do
/// and which keeps `Time64` a pure function of `t0` and [`SimTime`].
pub const IEEE1609_EPOCH_UNIX_S: i64 = 1_072_915_200;

/// Seconds in one day, used by the civil-date conversions.
const SECONDS_PER_DAY: i64 = 86_400;

/// Errors raised by time parsing and conversion.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TimeError {
    /// A timestamp string was not of the accepted form `YYYY-MM-DDTHH:MM:SSZ`.
    #[error("not an RFC 3339 UTC timestamp (expected YYYY-MM-DDTHH:MM:SSZ): {0:?}")]
    BadTimestamp(String),
    /// A civil datetime had an out-of-range field (month 0, day 32, hour 24, …).
    #[error("civil datetime field out of range: {0}")]
    BadCivilField(&'static str),
    /// The requested instant is before the IEEE 1609.2 epoch, so it has no 1609.2 value.
    #[error("instant is before the IEEE 1609.2 epoch (2004-01-01T00:00:00Z)")]
    BeforeIeeeEpoch,
}

/// Converts `seconds` to [`SimTime`], rounding to the nearest nanosecond.
///
/// Negative and NaN inputs clamp to `0`, and inputs beyond `u64::MAX` nanoseconds
/// saturate: simulated time cannot run before `t0`, and a model that computes a negative
/// duration has a bug the kernel must not amplify into a wrap-around.
pub fn secs_to_ns(seconds: f64) -> SimTime {
    if seconds.is_nan() || seconds <= 0.0 {
        // Covers negatives, -0.0 and NaN.
        return 0;
    }
    let ns = (seconds * (NS_PER_S as f64)).round();
    if ns >= (u64::MAX as f64) {
        u64::MAX
    } else {
        ns as SimTime
    }
}

/// Converts [`SimTime`] to seconds as `f64`.
///
/// Exact for the first 2⁵³ nanoseconds (about 104 days); beyond that the conversion
/// rounds, which is why the engine keeps time in integers and uses this only for model
/// inputs and display.
pub fn ns_to_secs(t: SimTime) -> f64 {
    (t as f64) / (NS_PER_S as f64)
}

/// Formats a [`SimTime`] as `H:MM:SS.nnnnnnnnn` for logs and the UI.
///
/// The hour field is unbounded (a 30-hour run prints `30:00:00.000000000`), the minute
/// and second fields are zero-padded to two digits and the fraction is always nine
/// digits, so formatted times sort lexicographically in the same order as they compare
/// numerically for runs shorter than ten hours.
pub fn format_sim_time(t: SimTime) -> String {
    let secs = t / NS_PER_S;
    let nanos = t % NS_PER_S;
    let hours = secs / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    format!("{hours}:{minutes:02}:{seconds:02}.{nanos:09}")
}

/// A civil (proleptic Gregorian, UTC) date and time, to the second.
///
/// Used for the scenario `t0` and for display. Conversions use Howard Hinnant's
/// `days_from_civil` / `civil_from_days` algorithms, which are exact integer arithmetic
/// and therefore identical on every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CivilDateTime {
    /// Proleptic Gregorian year (may be negative).
    pub year: i32,
    /// Month, 1–12.
    pub month: u8,
    /// Day of month, 1–31.
    pub day: u8,
    /// Hour, 0–23.
    pub hour: u8,
    /// Minute, 0–59.
    pub minute: u8,
    /// Second, 0–59. Leap seconds are not modelled.
    pub second: u8,
}

impl CivilDateTime {
    /// Builds a civil datetime, checking every field's range.
    pub fn new(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, TimeError> {
        if !(1..=12).contains(&month) {
            return Err(TimeError::BadCivilField("month"));
        }
        if day < 1 || u32::from(day) > days_in_month(year, month) {
            return Err(TimeError::BadCivilField("day"));
        }
        if hour > 23 {
            return Err(TimeError::BadCivilField("hour"));
        }
        if minute > 59 {
            return Err(TimeError::BadCivilField("minute"));
        }
        if second > 59 {
            return Err(TimeError::BadCivilField("second"));
        }
        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// Parses `YYYY-MM-DDTHH:MM:SSZ` (the scenario `time.t0` form, 03-interfaces.md §13).
    ///
    /// A lower-case `t`/`z` and a space instead of `T` are accepted; offsets other than
    /// `Z` and fractional seconds are not, because the scenario schema specifies UTC to
    /// the second.
    pub fn parse_rfc3339(s: &str) -> Result<Self, TimeError> {
        let bad = || TimeError::BadTimestamp(s.to_string());
        let b = s.as_bytes();
        if b.len() != 20 {
            return Err(bad());
        }
        let sep = b[10];
        if !(sep == b'T' || sep == b't' || sep == b' ') {
            return Err(bad());
        }
        if !(b[19] == b'Z' || b[19] == b'z') || b[4] != b'-' || b[7] != b'-' {
            return Err(bad());
        }
        if b[13] != b':' || b[16] != b':' {
            return Err(bad());
        }
        let num = |from: usize, to: usize| -> Result<i64, TimeError> {
            s.get(from..to)
                .ok_or_else(bad)?
                .parse::<i64>()
                .map_err(|_| bad())
        };
        // `parse::<i64>` accepts a leading `+`/`-`; reject anything non-digit explicitly.
        if !b[0..4].iter().all(u8::is_ascii_digit)
            || !b[5..7].iter().all(u8::is_ascii_digit)
            || !b[8..10].iter().all(u8::is_ascii_digit)
            || !b[11..13].iter().all(u8::is_ascii_digit)
            || !b[14..16].iter().all(u8::is_ascii_digit)
            || !b[17..19].iter().all(u8::is_ascii_digit)
        {
            return Err(bad());
        }
        Self::new(
            num(0, 4)? as i32,
            num(5, 7)? as u8,
            num(8, 10)? as u8,
            num(11, 13)? as u8,
            num(14, 16)? as u8,
            num(17, 19)? as u8,
        )
        .map_err(|_| bad())
    }

    /// Formats as `YYYY-MM-DDTHH:MM:SSZ`.
    pub fn to_rfc3339(self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// Seconds since the Unix epoch (1970-01-01T00:00:00Z).
    pub fn to_unix_seconds(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * SECONDS_PER_DAY
            + i64::from(self.hour) * 3_600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    /// Civil datetime of a Unix timestamp.
    pub fn from_unix_seconds(unix_s: i64) -> Self {
        let days = unix_s.div_euclid(SECONDS_PER_DAY);
        let rem = unix_s.rem_euclid(SECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (rem / 3_600) as u8,
            minute: ((rem % 3_600) / 60) as u8,
            second: (rem % 60) as u8,
        }
    }
}

impl core::fmt::Display for CivilDateTime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

/// The scenario wall clock: the civil datetime that simulated time zero stands for.
///
/// Stored as `i64` Unix seconds, so it is a plain integer in the manifest and in every
/// record. It converts a [`SimTime`] into a Unix timestamp, a civil datetime, or the
/// IEEE 1609.2 `Time32`/`Time64` values that certificates, signed messages and CRLs
/// carry ([`WallClock::time32`], [`WallClock::time64`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WallClock {
    /// Unix seconds of scenario time `t0`.
    t0_unix_s: i64,
}

impl WallClock {
    /// Builds a wall clock from the Unix timestamp of `t0`.
    pub const fn new(t0_unix_s: i64) -> Self {
        Self { t0_unix_s }
    }

    /// Builds a wall clock from a civil `t0`.
    pub fn from_civil(t0: CivilDateTime) -> Self {
        Self::new(t0.to_unix_seconds())
    }

    /// Parses `t0` from `YYYY-MM-DDTHH:MM:SSZ` (the scenario `time.t0` field).
    pub fn parse_rfc3339(t0: &str) -> Result<Self, TimeError> {
        Ok(Self::from_civil(CivilDateTime::parse_rfc3339(t0)?))
    }

    /// Unix seconds of `t0`.
    pub const fn t0_unix_s(self) -> i64 {
        self.t0_unix_s
    }

    /// Civil datetime of `t0`.
    pub fn t0_civil(self) -> CivilDateTime {
        CivilDateTime::from_unix_seconds(self.t0_unix_s)
    }

    /// Unix seconds at simulated time `t` (truncating towards `t0`).
    pub const fn unix_seconds_at(self, t: SimTime) -> i64 {
        self.t0_unix_s + (t / NS_PER_S) as i64
    }

    /// Unix nanoseconds at simulated time `t`, as `i128` to keep the full range.
    pub const fn unix_nanos_at(self, t: SimTime) -> i128 {
        self.t0_unix_s as i128 * NS_PER_S as i128 + t as i128
    }

    /// Civil datetime at simulated time `t`.
    pub fn civil_at(self, t: SimTime) -> CivilDateTime {
        CivilDateTime::from_unix_seconds(self.unix_seconds_at(t))
    }

    /// IEEE 1609.2 `Time32` at simulated time `t`: **seconds** since the 1609.2 epoch,
    /// 2004-01-01T00:00:00Z (not the Unix epoch).
    ///
    /// Returns [`TimeError::BeforeIeeeEpoch`] if the scenario `t0` places the instant
    /// before that epoch, and saturates at `u32::MAX` (year 2140).
    pub fn time32(self, t: SimTime) -> Result<u32, TimeError> {
        let secs = self.unix_seconds_at(t) - IEEE1609_EPOCH_UNIX_S;
        if secs < 0 {
            return Err(TimeError::BeforeIeeeEpoch);
        }
        Ok(u32::try_from(secs).unwrap_or(u32::MAX))
    }

    /// IEEE 1609.2 `Time64` at simulated time `t`: **microseconds** since the 1609.2
    /// epoch, 2004-01-01T00:00:00Z (not the Unix epoch).
    ///
    /// Returns [`TimeError::BeforeIeeeEpoch`] if the instant precedes that epoch. The
    /// sub-second part comes from `t` itself, truncated to whole microseconds, which is
    /// the resolution 1609.2 carries and the resolution the engine guarantees models.
    pub fn time64(self, t: SimTime) -> Result<u64, TimeError> {
        let whole_s = self.unix_seconds_at(t) - IEEE1609_EPOCH_UNIX_S;
        if whole_s < 0 {
            return Err(TimeError::BeforeIeeeEpoch);
        }
        let sub_us = (t % NS_PER_S) / NS_PER_US;
        Ok((whole_s as u64)
            .saturating_mul(1_000_000)
            .saturating_add(sub_us))
    }

    /// Formats the instant at simulated time `t` as `YYYY-MM-DDTHH:MM:SSZ`.
    pub fn to_rfc3339_at(self, t: SimTime) -> String {
        self.civil_at(t).to_rfc3339()
    }
}

impl Default for WallClock {
    /// The IEEE 1609.2 epoch, so a scenario that forgets `t0` still produces valid
    /// 1609.2 times rather than an error.
    fn default() -> Self {
        Self::new(IEEE1609_EPOCH_UNIX_S)
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's `days_from_civil`).
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = mp + if mp < 10 { 3 } else { -9 }; // [1, 12]
    ((y + i64::from(m <= 2)) as i32, m as u8, d as u8)
}

/// Number of days in a month of the proleptic Gregorian calendar.
fn days_in_month(year: i32, month: u8) -> u32 {
    const LENGTHS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    if month == 2 && leap {
        29
    } else {
        LENGTHS[(month - 1) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_constants() {
        assert_eq!(NS_PER_US, 1_000);
        assert_eq!(NS_PER_MS, 1_000 * NS_PER_US);
        assert_eq!(NS_PER_S, 1_000 * NS_PER_MS);
    }

    #[test]
    fn seconds_round_trip() {
        assert_eq!(secs_to_ns(1.5), 1_500_000_000);
        assert_eq!(secs_to_ns(0.0000001), 100);
        assert_eq!(secs_to_ns(-3.0), 0);
        assert_eq!(secs_to_ns(f64::NAN), 0);
        assert_eq!(secs_to_ns(f64::INFINITY), u64::MAX);
        assert!((ns_to_secs(1_500_000_000) - 1.5).abs() < 1e-15);
        assert_eq!(ns_to_secs(secs_to_ns(0.1)), 0.1);
    }

    /// The constructors, the unit conversions and the round trip through seconds.
    #[test]
    fn durations_convert_between_units() {
        assert_eq!(Duration::from_nanos(5).as_nanos(), 5);
        assert_eq!(Duration::from_micros(3), Duration::from_nanos(3_000));
        assert_eq!(Duration::from_millis(100), Duration::from_micros(100_000));
        assert_eq!(Duration::from_secs(2), Duration::from_millis(2_000));
        assert_eq!(Duration::from_secs_f64(1.5), Duration::from_millis(1_500));
        assert_eq!(Duration::from_secs_f64(-1.0), Duration::ZERO);
        assert_eq!(Duration::from_secs_f64(f64::NAN), Duration::ZERO);
        assert_eq!(Duration::from_millis(1_500).as_secs_f64(), 1.5);
        assert!(Duration::ZERO.is_zero());
        assert!(!Duration::from_nanos(1).is_zero());
        assert_eq!(Duration::default(), Duration::ZERO);
        assert_eq!(u64::from(Duration::from_secs(1)), NS_PER_S);
        assert_eq!(Duration::from_millis(100).to_string(), "0.100000000s");
        assert_eq!(Duration::from_secs(90).to_string(), "90.000000000s");
        assert_eq!(
            serde_json::to_string(&Duration::from_nanos(7)).unwrap(),
            "7",
            "a duration is a plain integer on the wire"
        );
        assert_eq!(
            serde_json::from_str::<Duration>("7").unwrap(),
            Duration::from_nanos(7)
        );
        // Ordering is by length, which is what a timer wheel or a sorted deadline list wants.
        let mut v = [
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_millis(1),
        ];
        v.sort();
        assert_eq!(v[0], Duration::ZERO);
        assert_eq!(v[2], Duration::from_secs(1));
    }

    /// "Now plus this much" is the operation models actually want, and it must not wrap in
    /// a release build: an event scheduled at a wrapped instant would fire *immediately*
    /// and in the wrong order, which is precisely the failure the scheduler's assertions
    /// exist to prevent.
    #[test]
    fn duration_arithmetic_saturates_instead_of_wrapping() {
        let now: SimTime = 10 * NS_PER_S;
        assert_eq!(
            now + Duration::from_millis(100),
            10 * NS_PER_S + 100 * NS_PER_MS
        );
        assert_eq!(
            Duration::from_millis(100).after(now),
            now + Duration::from_millis(100)
        );
        assert_eq!(now - Duration::from_secs(1), 9 * NS_PER_S);
        assert_eq!(
            now - Duration::from_secs(99),
            0,
            "time has no negative part"
        );

        assert_eq!(u64::MAX - 5 + Duration::from_secs(1), u64::MAX);
        assert_eq!(Duration::MAX.after(1), u64::MAX);
        assert_eq!(Duration::MAX + Duration::MAX, Duration::MAX);
        assert_eq!(Duration::from_secs(u64::MAX), Duration::MAX);
        assert_eq!(Duration::from_millis(1) * u64::MAX, Duration::MAX);

        assert_eq!(
            Duration::from_secs(1) - Duration::from_millis(250),
            Duration::from_millis(750)
        );
        assert_eq!(
            Duration::from_millis(1) - Duration::from_secs(1),
            Duration::ZERO,
            "a span cannot be negative"
        );
        assert_eq!(Duration::from_millis(100) * 3, Duration::from_millis(300));
        assert_eq!(
            Duration::from_millis(100).checked_div(4),
            Some(Duration::from_millis(25))
        );
        // Division by zero is the one operation that cannot be performed, and it says so
        // rather than aborting the run.
        assert_eq!(Duration::from_millis(100).checked_div(0), None);
        assert_eq!(Duration::ZERO.checked_div(0), None);
        assert_eq!(
            Duration::from_millis(100).saturating_div(4),
            Duration::from_millis(25)
        );
        assert_eq!(
            Duration::from_millis(100).saturating_div(0),
            Duration::MAX,
            "dividing by nothing saturates at the end of time"
        );
        // `const fn` in a const context: the old body was a compile error for k = 0.
        const DIVIDED: Option<Duration> = Duration::from_secs(1).checked_div(0);
        const SATURATED: Duration = Duration::from_secs(1).saturating_div(0);
        assert_eq!(DIVIDED, None);
        assert_eq!(SATURATED, Duration::MAX);

        let mut d = Duration::from_millis(100);
        d += Duration::from_millis(50);
        assert_eq!(d, Duration::from_millis(150));
        d -= Duration::from_millis(200);
        assert_eq!(d, Duration::ZERO);

        // The gap between two instants, and the clamp when they are out of order.
        assert_eq!(
            Duration::between(NS_PER_S, 3 * NS_PER_S),
            Duration::from_secs(2)
        );
        assert_eq!(Duration::between(3 * NS_PER_S, NS_PER_S), Duration::ZERO);
    }

    #[test]
    fn formats_sim_time() {
        assert_eq!(format_sim_time(0), "0:00:00.000000000");
        assert_eq!(format_sim_time(NS_PER_MS), "0:00:00.001000000");
        assert_eq!(
            format_sim_time(3_723 * NS_PER_S + 456_789_123),
            "1:02:03.456789123"
        );
    }

    #[test]
    fn civil_round_trips_known_dates() {
        // The IEEE 1609.2 epoch and the Unix epoch.
        assert_eq!(
            CivilDateTime::from_unix_seconds(IEEE1609_EPOCH_UNIX_S),
            CivilDateTime::new(2004, 1, 1, 0, 0, 0).unwrap()
        );
        assert_eq!(CivilDateTime::from_unix_seconds(0).year, 1970);
        // A leap day and a pre-Unix date.
        for unix in [-86_400_i64, 0, 951_782_400, 1_072_915_200, 1_772_409_600] {
            let c = CivilDateTime::from_unix_seconds(unix);
            assert_eq!(c.to_unix_seconds(), unix, "round trip for {unix}");
        }
        assert_eq!(
            CivilDateTime::from_unix_seconds(951_782_400).to_rfc3339(),
            "2000-02-29T00:00:00Z"
        );
    }

    #[test]
    fn parses_and_rejects_timestamps() {
        let t = CivilDateTime::parse_rfc3339("2027-03-04T07:00:00Z").unwrap();
        assert_eq!(t, CivilDateTime::new(2027, 3, 4, 7, 0, 0).unwrap());
        assert_eq!(t.to_rfc3339(), "2027-03-04T07:00:00Z");
        for bad in [
            "2027-03-04T07:00:00+01:00",
            "2027-03-04 07:00:00.5Z",
            "2027-13-04T07:00:00Z",
            "2027-02-30T07:00:00Z",
            "not a time",
            "20270304T070000Z",
        ] {
            assert!(
                CivilDateTime::parse_rfc3339(bad).is_err(),
                "should reject {bad:?}"
            );
        }
        assert!(CivilDateTime::parse_rfc3339("2027-03-04 07:00:00Z").is_ok());
    }

    #[test]
    fn wall_clock_maps_sim_time() {
        let w = WallClock::parse_rfc3339("2027-03-04T07:00:00Z").unwrap();
        assert_eq!(w.t0_civil().to_rfc3339(), "2027-03-04T07:00:00Z");
        assert_eq!(
            w.to_rfc3339_at(3_600 * NS_PER_S + 1),
            "2027-03-04T08:00:00Z"
        );
        assert_eq!(w.unix_seconds_at(NS_PER_S), w.t0_unix_s() + 1);
        assert_eq!(
            w.unix_nanos_at(5),
            i128::from(w.t0_unix_s()) * 1_000_000_000 + 5
        );
    }

    #[test]
    fn ieee_1609_2_times() {
        // t0 exactly one day after the 1609.2 epoch.
        let w = WallClock::parse_rfc3339("2004-01-02T00:00:00Z").unwrap();
        assert_eq!(w.time32(0).unwrap(), 86_400);
        assert_eq!(w.time64(0).unwrap(), 86_400_000_000);
        // Sub-second part is microseconds, truncated.
        assert_eq!(w.time64(1_500_999).unwrap(), 86_400_000_000 + 1_500);
        assert_eq!(w.time32(1_500_999).unwrap(), 86_400);
        // Before the epoch has no 1609.2 representation.
        let old = WallClock::parse_rfc3339("2003-12-31T23:59:59Z").unwrap();
        assert_eq!(old.time32(0), Err(TimeError::BeforeIeeeEpoch));
        assert_eq!(old.time64(0), Err(TimeError::BeforeIeeeEpoch));
        assert_eq!(old.time32(NS_PER_S).unwrap(), 0);
        // The default clock is the epoch itself.
        assert_eq!(WallClock::default().time32(0).unwrap(), 0);
    }

    #[test]
    fn wall_clock_serde_round_trip() {
        let w = WallClock::parse_rfc3339("2027-03-04T07:00:00Z").unwrap();
        let s = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<WallClock>(&s).unwrap(), w);
    }
}
