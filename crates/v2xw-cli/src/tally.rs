//! A [`RunRecorder`] that counts what goes past it on the way to another one.
//!
//! [`v2xw_engine::RunReport`] answers "how big was the run?" in events, frames and
//! records; it does not answer "what is *in* the recording?". A researcher asking whether
//! a scenario produced any signed transmissions at all wants the per-channel breakdown,
//! and reading it back out of the MCAP afterwards costs a second pass over the file for a
//! number the writer already had in its hands.
//!
//! [`Tally`] therefore sits in front of the real recorder and counts. It never filters,
//! never reorders and never rewrites, so wrapping a recorder cannot change what a run
//! records — a property `tests/tally.rs` asserts by digesting a run with and without it.

use std::collections::BTreeMap;

use v2xw_core::ctx::OwnedRecord;
use v2xw_core::time::SimTime;
use v2xw_engine::RunRecorder;

/// What one channel carried.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ChannelTally {
    /// How many records landed on it.
    pub records: u64,
    /// How many bytes of JSON those records carried, before compression.
    pub json_bytes: u64,
    /// The first instant one was written at, in nanoseconds.
    pub first_ns: u64,
    /// The last instant one was written at, in nanoseconds.
    pub last_ns: u64,
}

/// A recorder that counts and forwards.
#[derive(Debug)]
pub struct Tally<R: RunRecorder> {
    inner: R,
    by_channel: BTreeMap<String, ChannelTally>,
    /// Every `metric.sample` record, kept whole: a run's metrics are the one output a
    /// caller wants next to the recording rather than inside it.
    metric_samples: Vec<serde_json::Value>,
    records: u64,
}

impl<R: RunRecorder> Tally<R> {
    /// Wraps a recorder.
    pub fn new(inner: R) -> Self {
        Tally {
            inner,
            by_channel: BTreeMap::new(),
            metric_samples: Vec::new(),
            records: 0,
        }
    }

    /// The per-channel counts, in channel-name order.
    pub fn by_channel(&self) -> &BTreeMap<String, ChannelTally> {
        &self.by_channel
    }

    /// Every metric sample the run emitted, in emission order.
    pub fn metric_samples(&self) -> &[serde_json::Value] {
        &self.metric_samples
    }

    /// How many records went past, over every channel.
    pub fn records(&self) -> u64 {
        self.records
    }

    /// The wrapped recorder, for calls that are not record writes — writing the manifest
    /// metadata and adding an attachment, which belong to the container and not to the
    /// [`RunRecorder`] contract.
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Gives the wrapped recorder back, so it can be finished.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: RunRecorder> RunRecorder for Tally<R> {
    fn write(&mut self, at: SimTime, record: &OwnedRecord) {
        let e = self
            .by_channel
            .entry(record.channel.to_string())
            .or_insert(ChannelTally {
                first_ns: at,
                ..ChannelTally::default()
            });
        e.records += 1;
        e.json_bytes += record.json.len() as u64;
        e.last_ns = at;
        self.records += 1;
        if record.channel == "metric.sample"
            && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&record.json)
        {
            self.metric_samples.push(v);
        }
        self.inner.write(at, record);
    }

    fn refused(&self) -> u64 {
        self.inner.refused()
    }
}
