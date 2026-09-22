//! `v2xw-wasm` — the replay reader, compiled to WebAssembly.
//!
//! 09-ui §7 fixes the rule this crate exists to keep:
//!
//! > The replay reader is the same Rust code compiled natively (served by
//! > `v2xw serve --replay file.mcap`) or to WASM (static hosting), and emits the
//! > identical VWP stream, so the Studio has no replay-specific code path.
//!
//! So there is **no reader in here**. [`v2xw_record`] holds the container, the seek index
//! and the frame decoders; this crate compiles them for `wasm32-unknown-unknown`, adds
//! the two things a browser needs that a file system does not — fetching a recording a
//! range at a time, and handing pose columns to JavaScript without copying them — and
//! wraps the result in a `wasm-bindgen` surface.
//!
//! # The three pieces
//!
//! | Piece | Module | Why it is not in `v2xw-record` |
//! |---|---|---|
//! | Range-fetched byte source | [`ranges`] | A browser cannot read synchronously; the ask-and-retry loop is a transport concern |
//! | Resolved pose columns | [`scene`] | A renderer wants the state *at* `t`, which is the keyframe with its deltas applied |
//! | Seek loop | [`session`] | Ties the two together and is what the JavaScript class calls |
//! | JavaScript surface | `js` (`wasm32` only) | `wasm-bindgen` has no meaning on the host |
//!
//! # Two targets, one source
//!
//! Everything but the `wasm-bindgen` glue compiles for the host as well, and
//! `cargo test -p v2xw-wasm` runs the seek, the delta application and the range loop
//! natively against a recording [`v2xw_record::fixture`] wrote. The browser build adds no
//! logic to test separately; `tests/node/smoke.mjs` then proves the two agree on the same
//! file, field for field.
//!
//! Building for the browser needs a `clang` with the WebAssembly back end, because the
//! container's zstd codec is C. Apple's `clang` does not have one; `scripts/build-wasm.sh`
//! in this crate names the toolchain and the include shim, and the crate's `README.md`
//! says why.
//!
//! # The rules this crate is written under
//!
//! * **No wall clock.** Nothing here reads one. The seek benchmark is JavaScript
//!   (`bench/seek.mjs`) and lives outside the crate for exactly that reason.
//! * **No randomness.** There is none to have: a seek is a pure function of the file and
//!   the target time.
//! * **No `std` hash container reaches an ordering.** The byte cache's resident blocks
//!   and its wanted ranges are a `BTreeMap` and a `BTreeSet`, so the range requests a
//!   given scrub issues are the same every time and can be asserted on.
//! * **No `unsafe`.** The zero-copy handoff is a pointer and a length crossing to
//!   JavaScript, which is safe on this side; [`scene::Scene::generation`] is how the other
//!   side learns when its views went stale.
//! * No float is quantised here because none is produced here: every column is the wire's
//!   integer, and the only `f64` in the surface is the keyframe's own origin.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod ranges;
pub mod scene;
pub mod session;

#[cfg(all(target_arch = "wasm32", feature = "js"))]
pub mod js;

pub use ranges::{ByteCache, CachedSource, SharedCache};
pub use scene::Scene;
pub use session::{ReplaySession, SeekReport, Step};

pub use v2xw_core::time::{NS_PER_S, SimTime};

/// The simulated time in nanoseconds nearest to `seconds`.
///
/// Seek targets arrive from a scrub bar as a floating-point number of seconds. Rounding
/// is half-away-from-zero through [`v2xw_record::quant::round_half_away_from_zero`] — the
/// project's one rounding rule (ADR 0004) — rather than `as u64`'s truncation, so the
/// nanosecond a seek lands on does not depend on which side of the boundary the mouse
/// was. Out-of-range and non-finite inputs clamp instead of wrapping.
pub fn seconds_to_ns(seconds: f64) -> SimTime {
    if seconds.is_nan() || seconds <= 0.0 {
        return 0;
    }
    let ns = v2xw_record::quant::round_half_away_from_zero(seconds * NS_PER_S as f64);
    if ns >= u64::MAX as f64 {
        u64::MAX
    } else {
        ns as u64
    }
}
