//! The VWP v1 wire conformance suite: vwp-v1.md §10, mechanised.
//!
//! §10's own words are that "the test-kit ids match `tests/conformance/vwp/`", and this is
//! that directory. Every test here is named for the clause it checks — `f1_…`, `q4_…`,
//! `r10_…` — so a failure names the item without anyone having to look it up, which is the
//! whole reason the ids exist.
//!
//! One binary rather than nine files, for a practical reason: nine integration-test targets
//! are nine linked binaries, and the machine this repository is built on has run out of
//! memory doing less.
//!
//! # What is here and what is not
//!
//! The suite mechanises the items that span crates or that nobody owned, and *points at*
//! the owner of the rest through [`v2xw_conformance::checklist::COVERAGE`]. The pointer is
//! not decoration: [`coverage`] opens each named file and fails if the test it names has
//! been renamed or deleted. An item with no owner fails outright.
//!
//! Items that no in-process test can establish — a 1000 ms deadline on a live socket, a
//! p95 over a hundred seeks, a browser rendering a frame — are recorded as gaps with the
//! reason stated, and their count is pinned. That is the honest answer, and it is worth
//! more than a test that passes because it looks at nothing.

mod content;
mod control;
mod coverage;
mod framing;
mod handshake;
mod pose;
mod versioning;
mod visibility;
mod world;
