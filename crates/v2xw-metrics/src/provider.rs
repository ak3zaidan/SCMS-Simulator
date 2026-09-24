//! The [`MetricProvider`] seam and the set that drives a run's providers.
//!
//! 03-interfaces.md §10 publishes the trait as
//!
//! ```text
//! pub trait MetricProvider: Model {
//!     fn defs(&self) -> Vec<MetricDef>;
//!     fn subscribe(&self) -> Vec<ChannelName>;
//!     fn on_event(&mut self, ev: &EventRecord);
//!     fn flush(&mut self, at: SimTime) -> Vec<MetricSample>;
//! }
//! ```
//!
//! and that is the trait below, unchanged, with three defaulted methods on top that cost an
//! implementer nothing.
//!
//! # Why `on_event` cannot fail, and what happens when it does
//!
//! `on_event` returns `()`, so a provider has nowhere to put a record that does not decode.
//! Swallowing it silently is the wrong answer: a metric computed over half its input, with
//! no indication, is worse than no metric. Every provider in this crate therefore counts
//! what it could not read and reports it through [`MetricProvider::rejected`], and
//! [`ProviderSet::rejected_total`] surfaces the sum for the run manifest. A run whose
//! provider rejected records has a schema mismatch to fix, and it says so.
//!
//! # Registration
//!
//! A provider is registered through `v2xw_core::Registry` like any other plug-in
//! ([`ProviderSet::register`]), which validates its model card, checks the card's
//! `api_version` against this engine build, rejects a duplicate id and applies the licence
//! gate of ADR 0007 §5.
//!
//! What goes into the registry is the provider's **card**, not a live `ModelHandle`. The
//! registry's handle type is `Arc<dyn Model + Send + Sync>` — immutable and shared — and a
//! metric provider is mutated on every event, so the handle could never be the same object
//! as the one the run feeds. Registering the card pins the content hash the manifest needs
//! (`Registry::register` makes every check `register_model` makes), and the [`ProviderSet`]
//! owns the live object. `Registry::get_model` therefore answers `None` for a metric
//! provider, which is the honest answer: the live object is the set's, and its card is the
//! registry's.

use std::any::{Any, TypeId};
use std::cell::OnceCell;
use std::collections::BTreeMap;

use v2xw_core::ctx::{ChannelName, EventRecord};
use v2xw_core::model::Model;
use v2xw_core::registry::{Licence, ModelRef, Registry};
use v2xw_core::time::SimTime;

use crate::channels::{ChannelView, decode};
use crate::def::{MetricDef, MetricSample};
use crate::error::Result;

/// One recorded event, decoded at most once however many providers read it.
///
/// A record reaches every provider subscribed to its channel, and every one of them used
/// to parse its JSON for itself: a `node.rx` record was decoded four times (by the
/// communication, latency, awareness and load providers), a `node.tx` four times and a
/// `gt.kinematics` three. [`ProviderSet::on_event`] now builds one `Decoded` per record and
/// hands the same one to every subscriber, and [`Decoded::with`] decodes on first use and
/// caches the result — a failure included, so a record that does not fit its view is
/// refused by every provider that asks, exactly as before.
///
/// The cache holds one view type per record. A record is on exactly one channel and every
/// provider in this crate reads a channel through that channel's one view, so one slot is
/// all it needs; a provider that reads the same channel through a *different* view type
/// still works — it is decoded for that caller alone, uncached — so the cache can never
/// hand a provider a value of the wrong type.
pub struct Decoded<'a> {
    record: &'a EventRecord,
    slot: OnceCell<(TypeId, Option<Box<dyn Any>>)>,
}

impl core::fmt::Debug for Decoded<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Decoded")
            .field("channel", &self.record.channel)
            .field("decoded", &self.slot.get().is_some())
            .finish()
    }
}

impl<'a> Decoded<'a> {
    /// A record not yet decoded.
    #[must_use]
    pub fn new(record: &'a EventRecord) -> Self {
        Self {
            record,
            slot: OnceCell::new(),
        }
    }

    /// The record itself.
    #[must_use]
    pub const fn record(&self) -> &'a EventRecord {
        self.record
    }

    /// The record's channel.
    #[must_use]
    pub const fn channel(&self) -> &'static str {
        self.record.channel
    }

    /// Whether the record has been decoded yet, for a test that counts decodes.
    #[must_use]
    pub fn is_decoded(&self) -> bool {
        self.slot.get().is_some()
    }

    /// Runs `f` over the record as its channel's view `V`: `Some` when it decodes, `None`
    /// when it does not or is on another channel. The first call for a record decodes;
    /// every later call for the same `V` reads the cached value.
    pub fn with<V, R>(&self, f: impl FnOnce(Option<&V>) -> R) -> R
    where
        V: ChannelView + 'static,
    {
        if self.record.channel != V::CHANNEL {
            return f(None);
        }
        let (ty, value) = self.slot.get_or_init(|| {
            (
                TypeId::of::<V>(),
                decode::<V>(self.record)
                    .ok()
                    .map(|v| Box::new(v) as Box<dyn Any>),
            )
        });
        if *ty == TypeId::of::<V>() {
            return f(value.as_ref().and_then(|b| b.downcast_ref::<V>()));
        }
        let own = decode::<V>(self.record).ok();
        f(own.as_ref())
    }
}

/// A provider of one family of metrics (03-interfaces.md §10).
///
/// Extends `Model`, so every provider carries a model card citing the definition of the
/// metric it computes — 08-measurement-and-data.md §1: "Every metric is computed by a
/// `MetricProvider` with a `MetricDef` (name, unit, dimensions, aggregation, visibility,
/// definition text, source)".
pub trait MetricProvider: Model {
    /// The metrics this provider computes.
    fn defs(&self) -> Vec<MetricDef>;

    /// The channels it needs. A `ChannelName` comes from the record type, so it cannot be
    /// misspelled in a string literal.
    fn subscribe(&self) -> Vec<ChannelName>;

    /// Consumes one recorded event.
    ///
    /// The provider is handed every record on a channel it subscribed to, in the order the
    /// run produced them. A record it cannot decode is counted into
    /// [`MetricProvider::rejected`] rather than dropped in silence.
    fn on_event(&mut self, ev: &EventRecord);

    /// Consumes one recorded event that may already have been decoded for another
    /// provider ([`Decoded`]). [`ProviderSet`] calls this rather than
    /// [`MetricProvider::on_event`]; the default forwards to `on_event`, so a provider
    /// that does not override it decodes for itself exactly as before.
    fn on_decoded(&mut self, ev: &Decoded<'_>) {
        self.on_event(ev.record());
    }

    /// Emits the samples for the window that ends at `at`, and starts a new window.
    ///
    /// "Starts a new window" is part of the contract: a provider's per-window accumulators
    /// are drained here, so calling `flush` twice at the same instant produces the samples
    /// once and then an empty (or insufficient) set, never the same numbers twice.
    /// Whole-run accumulators — a revocation's stage timestamps, a detection's confusion
    /// matrix — are not drained, and each provider's documentation says which of its
    /// metrics are windowed and which are cumulative.
    fn flush(&mut self, at: SimTime) -> Vec<MetricSample>;

    /// How many records this provider could not decode. Defaults to zero for a provider
    /// that does not count.
    fn rejected(&self) -> u64 {
        0
    }

    /// Checks every definition this provider publishes.
    ///
    /// Defaulted in terms of [`MetricProvider::defs`] and `MetricDef::validate`, so a
    /// provider gets the check for free and the conformance kit has one call to make.
    ///
    /// # Errors
    /// The first [`crate::MetricError::BadDefinition`] among the provider's definitions.
    fn validate_defs(&self) -> Result<()> {
        for d in self.defs() {
            d.validate()?;
        }
        Ok(())
    }

    /// The definition of one of this provider's metrics, by name.
    fn def(&self, name: &str) -> Option<MetricDef> {
        self.defs().into_iter().find(|d| d.name == name)
    }
}

/// The providers of one run, with their channel subscriptions resolved.
///
/// Dispatch is by channel through a [`BTreeMap`], so a record reaches the subscribed
/// providers in a fixed order however many there are and whatever order they were
/// registered in. A `HashMap` here would make the order of two providers' side effects
/// depend on a hash seed; both providers see the same record either way, but a provider
/// that emits on `on_event` would emit in a seed-dependent order, and the determinism
/// contract does not allow that.
#[derive(Default)]
pub struct ProviderSet {
    providers: Vec<Box<dyn MetricProvider + Send>>,
    refs: Vec<ModelRef>,
    by_channel: BTreeMap<ChannelName, Vec<usize>>,
}

impl core::fmt::Debug for ProviderSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProviderSet")
            .field("providers", &self.providers.len())
            .field("channels", &self.by_channel.len())
            .finish_non_exhaustive()
    }
}

impl ProviderSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a provider's card with `registry` and takes ownership of the live object.
    ///
    /// Every definition the provider publishes is validated first, so a set cannot be built
    /// around a metric with no stated formula or no declared omission.
    ///
    /// # Errors
    /// [`crate::MetricError::BadDefinition`] if one of the provider's definitions is
    /// invalid, or the wrapped `RegistryError` if the card fails validation, targets another
    /// API version, duplicates an id or needs out-of-process hosting under its licence.
    pub fn register(
        &mut self,
        registry: &mut Registry,
        provider: Box<dyn MetricProvider + Send>,
    ) -> Result<ModelRef> {
        self.register_with_licence(registry, provider, Licence::Unspecified)
    }

    /// [`ProviderSet::register`] with the provider's licence declared.
    ///
    /// # Errors
    /// As [`ProviderSet::register`], plus `RegistryError::LicenceRequiresOutOfProcess` for
    /// a copyleft or unknown licence.
    pub fn register_with_licence(
        &mut self,
        registry: &mut Registry,
        provider: Box<dyn MetricProvider + Send>,
        licence: Licence,
    ) -> Result<ModelRef> {
        provider.validate_defs()?;
        let r = registry
            .register_with_licence(provider.card().clone(), licence)
            .map_err(|e| crate::error::MetricError::Core(e.into()))?;
        let index = self.providers.len();
        for ch in provider.subscribe() {
            self.by_channel.entry(ch).or_default().push(index);
        }
        self.providers.push(provider);
        self.refs.push(r);
        Ok(r)
    }

    /// How many providers the set holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// True if no provider is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Every channel any provider subscribed to, in a fixed order — what the recorder has
    /// to deliver.
    #[must_use]
    pub fn channels(&self) -> Vec<ChannelName> {
        self.by_channel.keys().copied().collect()
    }

    /// The registry references of the registered providers, in registration order.
    #[must_use]
    pub fn refs(&self) -> &[ModelRef] {
        &self.refs
    }

    /// Every definition every provider publishes, sorted by metric name.
    ///
    /// The catalog 08-measurement-and-data.md §1 says is generated from the definitions.
    /// Sorted rather than in registration order so that the generated page — and any digest
    /// over it — does not depend on the order the run happened to register providers in.
    #[must_use]
    pub fn catalog(&self) -> Vec<MetricDef> {
        let mut defs: Vec<MetricDef> = self.providers.iter().flat_map(|p| p.defs()).collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Delivers one record to every provider subscribed to its channel.
    ///
    /// A record on a channel nobody subscribed to is ignored, which is not an error: a
    /// recording carries every channel and a run's provider set is a subset of them.
    pub fn on_event(&mut self, ev: &EventRecord) {
        let Some(indices) = self.by_channel.get(&ev.channel_name()) else {
            return;
        };
        // Cloned because the borrow of `by_channel` would otherwise outlive the mutable
        // borrow of `providers`. The vector is one small `usize` per subscriber.
        let indices = indices.clone();
        let decoded = Decoded::new(ev);
        for i in indices {
            self.providers[i].on_decoded(&decoded);
        }
    }

    /// Flushes every provider and returns the samples, sorted by `(metric, key, t)`.
    ///
    /// The sort is what makes the set's output a function of the run rather than of the
    /// registration order: two runs that registered the same providers in different orders
    /// produce the same table. The key is the metric's name and dimension values
    /// (`MetricSample::key`), which is the same ordering the run summary and the digest use.
    pub fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out: Vec<MetricSample> = self
            .providers
            .iter_mut()
            .flat_map(|p| p.flush(at))
            .collect();
        out.sort_by(|a, b| a.key().cmp(&b.key()).then(a.t.cmp(&b.t)));
        out
    }

    /// The total number of records the set's providers could not decode.
    #[must_use]
    pub fn rejected_total(&self) -> u64 {
        self.providers.iter().map(|p| p.rejected()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::def::{Agg, Dims, SampleValue};
    use crate::quant::Quantum;
    use v2xw_core::card::{Family, ModelCard};
    use v2xw_core::ctx::{OwnedRecord, Visibility};

    struct Counting {
        card: ModelCard,
        name: &'static str,
        channel: &'static str,
        seen: u64,
    }

    impl Counting {
        fn new(id: &str, name: &'static str, channel: &'static str) -> Self {
            let mut card = ModelCard::new(id, Family::Metric, "1.0.0", "Counts records.");
            card.tier = vec![v2xw_core::card::Tier::Abstract];
            Self {
                card,
                name,
                channel,
                seen: 0,
            }
        }

        fn def(&self) -> MetricDef {
            MetricDef::new(
                self.name,
                "count",
                Agg::Count,
                Visibility::Node,
                Quantum::COUNT,
                "Records seen.",
            )
            .not_accounting_for("records on channels it did not subscribe to")
        }
    }

    impl Model for Counting {
        fn card(&self) -> &ModelCard {
            &self.card
        }
    }

    impl MetricProvider for Counting {
        fn defs(&self) -> Vec<MetricDef> {
            vec![self.def()]
        }

        fn subscribe(&self) -> Vec<ChannelName> {
            vec![ChannelName(self.channel)]
        }

        fn on_event(&mut self, _ev: &EventRecord) {
            self.seen += 1;
        }

        fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
            let n = core::mem::take(&mut self.seen);
            vec![MetricSample::new(
                &self.def(),
                at,
                Dims::new(),
                SampleValue::count(n),
            )]
        }
    }

    fn rec(channel: &'static str) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility: Visibility::Node,
            json: b"{}".to_vec(),
        }
    }

    #[test]
    fn registration_goes_through_the_core_registry() {
        let mut reg = Registry::new();
        let mut set = ProviderSet::new();
        let r = set
            .register(
                &mut reg,
                Box::new(Counting::new("metric/test/a", "a", "node.tx")),
            )
            .unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get_ref(r).unwrap().card.family, Family::Metric);
        assert!(
            reg.get_model(r).is_none(),
            "the live object is the set's, not the registry's"
        );
        assert_eq!(set.refs(), &[r]);
    }

    #[test]
    fn a_duplicate_id_is_refused_by_the_registry() {
        let mut reg = Registry::new();
        let mut set = ProviderSet::new();
        set.register(
            &mut reg,
            Box::new(Counting::new("metric/test/a", "a", "node.tx")),
        )
        .unwrap();
        assert!(
            set.register(
                &mut reg,
                Box::new(Counting::new("metric/test/a", "b", "node.tx"))
            )
            .is_err()
        );
    }

    #[test]
    fn dispatch_reaches_only_the_subscribers() {
        let mut reg = Registry::new();
        let mut set = ProviderSet::new();
        set.register(
            &mut reg,
            Box::new(Counting::new("metric/test/tx", "tx_seen", "node.tx")),
        )
        .unwrap();
        set.register(
            &mut reg,
            Box::new(Counting::new("metric/test/rx", "rx_seen", "phy.rx")),
        )
        .unwrap();
        assert_eq!(
            set.channels()
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>(),
            vec!["node.tx", "phy.rx"]
        );
        set.on_event(&rec("node.tx"));
        set.on_event(&rec("node.tx"));
        set.on_event(&rec("phy.rx"));
        set.on_event(&rec("mac.cbr")); // nobody subscribed; not an error
        let samples = set.flush(1_000);
        assert_eq!(samples.len(), 2);
        // Sorted by key, so `rx_seen` precedes `tx_seen` whatever the registration order.
        assert_eq!(samples[0].metric, "rx_seen");
        assert_eq!(samples[0].value, SampleValue::count(1));
        assert_eq!(samples[1].metric, "tx_seen");
        assert_eq!(samples[1].value, SampleValue::count(2));
    }

    #[test]
    fn flushing_twice_does_not_report_the_same_window_twice() {
        let mut reg = Registry::new();
        let mut set = ProviderSet::new();
        set.register(
            &mut reg,
            Box::new(Counting::new("metric/test/tx", "tx_seen", "node.tx")),
        )
        .unwrap();
        set.on_event(&rec("node.tx"));
        assert_eq!(set.flush(1_000)[0].value, SampleValue::count(1));
        assert_eq!(set.flush(2_000)[0].value, SampleValue::count(0));
    }

    #[test]
    fn the_catalog_is_sorted_by_name_not_by_registration_order() {
        let mut reg = Registry::new();
        let mut set = ProviderSet::new();
        set.register(
            &mut reg,
            Box::new(Counting::new("metric/test/z", "z_metric", "node.tx")),
        )
        .unwrap();
        set.register(
            &mut reg,
            Box::new(Counting::new("metric/test/a", "a_metric", "node.tx")),
        )
        .unwrap();
        let names: Vec<String> = set.catalog().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["a_metric", "z_metric"]);
    }

    thread_local! {
        static DECODES: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
    }

    /// A `node.rx` view that counts how many times it has been decoded.
    #[derive(Debug)]
    struct Probe;

    impl<'de> serde::Deserialize<'de> for Probe {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> core::result::Result<Self, D::Error> {
            let _ = serde::de::IgnoredAny::deserialize(d)?;
            DECODES.with(|c| c.set(c.get() + 1));
            Ok(Probe)
        }
    }

    impl ChannelView for Probe {
        const CHANNEL: &'static str = "node.rx";
    }

    /// A provider that reads `node.rx` through the shared decode.
    struct ProbeReader {
        inner: Counting,
    }

    impl Model for ProbeReader {
        fn card(&self) -> &ModelCard {
            self.inner.card()
        }
    }

    impl MetricProvider for ProbeReader {
        fn defs(&self) -> Vec<MetricDef> {
            self.inner.defs()
        }
        fn subscribe(&self) -> Vec<ChannelName> {
            self.inner.subscribe()
        }
        fn on_event(&mut self, ev: &EventRecord) {
            self.on_decoded(&Decoded::new(ev));
        }
        fn on_decoded(&mut self, ev: &Decoded<'_>) {
            if ev.with(|v: Option<&Probe>| v.is_some()) {
                self.inner.seen += 1;
            }
        }
        fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
            self.inner.flush(at)
        }
    }

    #[test]
    fn a_record_is_decoded_once_however_many_providers_read_it() {
        let mut reg = Registry::new();
        let mut set = ProviderSet::new();
        for (id, name) in [
            ("metric/test/p1", "p1"),
            ("metric/test/p2", "p2"),
            ("metric/test/p3", "p3"),
        ] {
            set.register(
                &mut reg,
                Box::new(ProbeReader {
                    inner: Counting::new(id, name, "node.rx"),
                }),
            )
            .unwrap();
        }
        DECODES.with(|c| c.set(0));
        set.on_event(&OwnedRecord {
            channel: "node.rx",
            visibility: Visibility::Node,
            json: br#"{"t":0}"#.to_vec(),
        });
        assert_eq!(
            DECODES.with(core::cell::Cell::get),
            1,
            "three subscribers, one decode"
        );
        // Every one of the three still saw the record.
        let seen: Vec<SampleValue> = set.flush(1).into_iter().map(|s| s.value).collect();
        assert_eq!(seen, vec![SampleValue::count(1); 3]);
    }

    #[test]
    fn a_second_view_type_of_one_channel_is_decoded_for_itself() {
        let rec = OwnedRecord {
            channel: "node.rx",
            visibility: Visibility::Node,
            json: br#"{"t":5,"rx":1,"outcome":"delivered"}"#.to_vec(),
        };
        let d = Decoded::new(&rec);
        assert!(!d.is_decoded());
        assert!(d.with(|v: Option<&Probe>| v.is_some()));
        assert!(d.is_decoded());
        // The cache holds a `Probe`; asking for the real view still gets the real view.
        let t = d.with(|v: Option<&crate::channels::NodeRxView>| v.map(|v| v.t));
        assert_eq!(t, Some(5));
        // And a record on another channel is `None`, not a mis-typed value.
        assert!(d.with(|v: Option<&crate::channels::NodeTxView>| v.is_none()));
    }
}
