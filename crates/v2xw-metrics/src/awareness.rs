//! Awareness and freshness: what each vehicle knows about its neighbours, and how old it is.
//!
//! Delivery ratio says whether a message arrived. A cooperative-awareness application
//! cares about something else: whether, at a given instant, it holds a *recent* state for
//! *every* vehicle near it. Three metrics answer that, all from `node.rx` (what each
//! receiver actually handed to its applications) joined to `gt.kinematics` (where every
//! vehicle really was):
//!
//! | Metric | Formula | Unit | Source |
//! |---|---|---|---|
//! | `aoi` | time-average age of information per directed link: ∫ (t − g(t)) dt / ∫ dt, where g(t) is the generation time of the freshest message the receiver holds from that sender | ms | Kaul, Gruteser, Rai, Kenney, "Minimizing age of information in vehicular networks", IEEE SECON 2011; Kaul, Yates, Gruteser, IEEE INFOCOM 2012 |
//! | `aoi_peak` | the age just before each update: t_delivered(k) − g(k−1) | ms (distribution) | Costa, Codreanu, Ephremides, "On the age of information in status update systems with packet management", IEEE Trans. Inf. Theory 62(4), 2016 |
//! | `nar` | Σ over receivers of (neighbours within R heard within T) / Σ (neighbours within R) | ratio | Boban and d'Orey, "Exploring the practical limits of cooperative awareness in vehicular communications", IEEE Trans. Veh. Technol. 65(6), 2016 (the neighbourhood awareness ratio); 08-measurement-and-data.md §2.1 (r = 100–300 m, T = 1 s) |
//! | `delivery_ratio` | messages delivered to the application / reception attempts resolved, per 50 m distance bin | ratio | 08-measurement-and-data.md §2.1 (`pdr` by `dist_bin`), applied at the application rather than the PHY |
//!
//! The inter-reception time `pir` (Martelli et al., IEEE INFOCOM 2012) is in
//! [`crate::comms`], where it has always been.
//!
//! # What is ground truth here
//!
//! All four read the sender's true identity, and `nar` reads true positions; every one is
//! tagged node-and-ground-truth (08-measurement-and-data.md §1). A real receiver cannot
//! compute them — it knows pseudonyms, not vehicles, and it does not know who it failed to
//! hear. That is exactly why a simulator should.

use std::collections::BTreeMap;

use serde_json::json;
use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::bins::Bins;
use crate::cards;
use crate::channels::{ChannelView, GtKinematicsView, NodeRxView, RxFate};
use crate::def::{Agg, DEFAULT_LEVEL, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::{Decoded, MetricProvider};
use crate::quant::Quantum;
use crate::stats::{ConfidenceLevel, Distribution, Proportion, ratio_of_sums};

/// The neighbourhood radii `nar` is reported at, metres: the two ends of 08 §2.1's range.
pub const NAR_RADII_M: [u32; 2] = [100, 300];

/// How recently a neighbour must have been heard to count as known, the `T` of `nar`.
pub const NAR_HORIZON: Duration = Duration::from_secs(1);

/// How many distinct neighbour pairs a window needs before `nar` reports an estimate:
/// one. A small sample is reported with the wide interval it deserves
/// ([`crate::stats::Proportion::estimate_clustered`]) rather than refused; only a window
/// with no neighbour pair at all — where the ratio has no denominator — is insufficient.
pub const NAR_MIN_PAIRS: u64 = 1;

/// How long a link may go unheard before its age-of-information sawtooth is restarted
/// rather than extended — a vehicle that drove away and came back is a new encounter.
pub const AOI_RESTART: Duration = Duration::from_secs(10);

/// How old a vehicle's last known position may be and still place it in a neighbourhood.
/// Two mobility steps at the slowest cadence a scenario may choose, so a vehicle that
/// despawned stops being anybody's neighbour.
const POSITION_STALE: Duration = Duration::from_secs(2);

/// Freshness state of one directed link.
#[derive(Debug, Clone, Copy)]
struct Link {
    /// When the freshest message was delivered.
    delivered: SimTime,
    /// Its generation time.
    generated: SimTime,
}

/// The awareness provider.
pub struct AwarenessProvider {
    card: ModelCard,
    level: ConfidenceLevel,
    min_samples: u64,
    bins: Bins,
    window_start: SimTime,

    /// Latest delivery per (receiver, sender), for `nar`.
    last_heard: BTreeMap<(NodeId, NodeId), SimTime>,
    /// Freshness per (receiver, sender), for `aoi`.
    links: BTreeMap<(NodeId, NodeId), Link>,
    /// Latest known true position per node: (instant, x, y).
    positions: BTreeMap<NodeId, (SimTime, f64, f64)>,

    /// Σ age × time over the window, ns², and Σ time, ns — exact in integers.
    aoi_area: u128,
    aoi_time: u128,
    aoi_updates: u64,
    aoi_peak_ms: Distribution,
    delivery_by_bin: BTreeMap<usize, Proportion>,
    delivery_unbinned: Proportion,
    delivery_all: Proportion,
    /// The kinematics instant being assembled, and the last instant `nar` was sampled at.
    frame_t: Option<SimTime>,
    nar_sampled_at: Option<SimTime>,
    /// `nar`'s pair observations over the window, per radius in [`NAR_RADII_M`] order, and
    /// the distinct (receiver, neighbour) pairs behind them — the clusters.
    nar_window: [Proportion; 2],
    nar_pairs: [std::collections::BTreeSet<(NodeId, NodeId)>; 2],
    rejected: u64,
}

impl AwarenessProvider {
    /// A provider with 50 m distance bins out to 1 km — the engine's candidate range — a
    /// 95 % level and the crate's insufficiency threshold. `t0` starts the first window.
    #[must_use]
    pub fn new(t0: SimTime) -> Self {
        Self {
            card: Self::build_card(),
            level: DEFAULT_LEVEL,
            min_samples: crate::stats::DEFAULT_MIN_SAMPLES,
            bins: Bins::uniform("dist_bin", "m", 50.0, 20, Quantum::LENGTH_M)
                .expect("50 m bins to 1 km are valid"),
            window_start: t0,
            last_heard: BTreeMap::new(),
            links: BTreeMap::new(),
            positions: BTreeMap::new(),
            aoi_area: 0,
            aoi_time: 0,
            aoi_updates: 0,
            aoi_peak_ms: Distribution::new(),
            delivery_by_bin: BTreeMap::new(),
            delivery_unbinned: Proportion::new(),
            delivery_all: Proportion::new(),
            frame_t: None,
            nar_sampled_at: None,
            nar_window: [Proportion::new(); 2],
            nar_pairs: Default::default(),
            rejected: 0,
        }
    }

    /// Sets the insufficiency threshold.
    #[must_use]
    pub const fn with_min_samples(mut self, n: u64) -> Self {
        self.min_samples = n;
        self
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/awareness/aoi-nar",
            "1.0.0",
            "Age of information per neighbour, neighbourhood awareness ratio and \
             application-level delivery ratio by distance.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "aoi",
                "aoi = Σ_links ∫ (t − g(t)) dt / Σ_links ∫ dt over the intervals between \
                 deliveries; one interval [d(k−1), d(k)) contributes (d(k) − d(k−1)) · \
                 ((d(k−1) + d(k))/2 − g(k−1))",
            ),
            v2xw_core::card::Equation::new("aoi_peak", "peak(k) = d(k) − g(k−1)"),
            v2xw_core::card::Equation::new(
                "nar",
                "nar(R) = Σ_i |{j : ‖p_i − p_j‖ ≤ R, t − last_heard(i, j) ≤ T}| / \
                 Σ_i |{j : ‖p_i − p_j‖ ≤ R}|",
            ),
            v2xw_core::card::Equation::new(
                "delivery_ratio",
                "delivery_ratio = delivered / (delivered + lost), per 50 m distance bin",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.parameters.push(cards::param(
            "nar_horizon_s",
            "s",
            json!(1.0),
            json!(0.1),
            json!(10.0),
            cards::design("08-measurement-and-data.md §2.1 (nar: T = 1 s)"),
        ));
        card.parameters.push(cards::param(
            "aoi_restart_s",
            "s",
            json!(10.0),
            json!(1.0),
            json!(3600.0),
            cards::design(
                "this crate's choice: a link unheard for longer than this starts a new \
                 encounter rather than extending one age sawtooth across the gap",
            ),
        ));
        card.sources = vec![
            cards::paper(
                "Kaul, Gruteser, Rai, Kenney, 'Minimizing age of information in vehicular \
                 networks', IEEE SECON 2011 (age of information defined for beaconing)",
            ),
            cards::paper(
                "Kaul, Yates, Gruteser, 'Real-time status: how often should one update?', \
                 IEEE INFOCOM 2012 (the time-average age as the area under the sawtooth)",
            ),
            cards::paper(
                "Costa, Codreanu, Ephremides, 'On the age of information in status update \
                 systems with packet management', IEEE Trans. Inf. Theory 62(4), 2016 (peak \
                 age of information)",
            ),
            cards::paper(
                "Boban and d'Orey, 'Exploring the practical limits of cooperative awareness \
                 in vehicular communications', IEEE Trans. Veh. Technol. 65(6), 2016 \
                 (neighbourhood awareness ratio). Cited from memory of the published \
                 definition; the exact equation number is unverified.",
            ),
            cards::design("08-measurement-and-data.md §2.1 (nar, r = 100–300 m, T = 1 s)"),
        ];
        card.limitations = vec![
            "aoi integrates only between deliveries on a link: the time after a link's last \
             delivery, when its age keeps growing, is not counted until the next delivery \
             (or at all, if there is none). A link that went silent therefore looks \
             fresher than it was; nar is the metric that sees silence."
                .to_string(),
            "nar counts vehicles whose true position is within R. A vehicle behind a \
             building is a neighbour here even if no radio could reach it, which is the \
             definition, not an error."
                .to_string(),
            "Roadside units are neither counted as neighbours nor as receivers in nar, \
             because they carry no gt.kinematics record."
                .to_string(),
            "nar is sampled at every mobility step and its interval assumes the samples of \
             one neighbour pair are fully correlated (the effective sample size is the number \
             of distinct pairs, Kish 1965). That is the conservative extreme: the true \
             interval is somewhat narrower. An instant is sampled when the next instant's \
             first record arrives, so a delivery up to one step after it can count as heard."
                .to_string(),
        ];
        card.ignores = vec![
            "Pseudonym changes: every metric here pairs on the sender's true node id.".to_string(),
        ];
        card.validation.tests = vec![
            "awareness::tests::aoi_of_a_periodic_link_is_half_the_period_plus_the_delay"
                .to_string(),
            "awareness::tests::nar_counts_heard_neighbours_within_the_radius".to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let src = cards::design("08-measurement-and-data.md §2.1");
        vec![
            MetricDef::new(
                "aoi",
                "ms",
                Agg::ratio("Σ age × time", "Σ time"),
                Visibility::NodeAndGt,
                Quantum::TIME_MS,
                "Age of information: how old, on average over time, the freshest message a \
                 receiver holds from each neighbour is. Integrated exactly between \
                 deliveries on every directed link and pooled over links.",
            )
            .with_dims([Dim::T])
            .with_source(cards::paper(
                "Kaul, Gruteser, Rai, Kenney, IEEE SECON 2011; Kaul, Yates, Gruteser, IEEE \
                 INFOCOM 2012",
            ))
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("the age after a link's last delivery, which keeps growing unseen")
            .not_accounting_for("pseudonym changes: links are keyed by the true sender"),
            MetricDef::new(
                "aoi_peak",
                "ms",
                Agg::Distribution,
                Visibility::NodeAndGt,
                Quantum::TIME_MS,
                "Peak age of information: the age of a receiver's information about a \
                 sender just before a new message from it arrives.",
            )
            .with_dims([Dim::T])
            .with_source(cards::paper(
                "Costa, Codreanu, Ephremides, IEEE Trans. Inf. Theory 62(4), 2016",
            ))
            .with_min_samples(self.min_samples)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("the first message on a link, which has no predecessor")
            .not_accounting_for("links restarted after a silence longer than aoi_restart_s"),
            MetricDef::new(
                "nar",
                "ratio",
                Agg::ratio("neighbours within R heard within T", "neighbours within R"),
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "Neighbourhood awareness ratio: of the vehicles truly within R metres of each \
                 equipped vehicle, the fraction it has received a message from in the last \
                 second. Reported at R = 100 m and R = 300 m. Sampled at every mobility step \
                 and pooled over the window; the 95 % Wilson interval is computed on the \
                 number of distinct neighbour pairs, not on the samples, because one pair \
                 seen ten times is not ten independent observations — so a sparse run \
                 reports an estimate with a wide interval instead of refusing.",
            )
            .with_dims([Dim::T, Dim::Radius])
            .with_breakdown(Dim::Radius, NAR_RADII_M.iter().map(|r| format!("{r}m")))
            .breakdown_only()
            .with_source(cards::paper(
                "Boban and d'Orey, IEEE Trans. Veh. Technol. 65(6), 2016; \
                 08-measurement-and-data.md §2.1; the interval: Brown, Cai, DasGupta, \
                 Statistical Science 16(2), 2001 (Wilson for small n) and Kish, Survey \
                 Sampling, 1965 (effective sample size of a clustered sample)",
            ))
            .with_min_samples(NAR_MIN_PAIRS)
            .with_range(0.0, 1.0)
            .not_accounting_for("roadside units, which have no ground-truth kinematics")
            .not_accounting_for(
                "whether the neighbour was reachable at all (it is counted either way)",
            ),
            MetricDef::new(
                "delivery_ratio",
                "ratio",
                Agg::ratio("messages delivered to the application", "attempts resolved"),
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "Application-level packet delivery ratio: of the reception attempts at \
                 candidate receivers, the fraction that reached the receiver's applications \
                 — after the PHY, reassembly, the receive queue, the verification policy and \
                 the signature check. With no dimension over every distance; `dist_bin` in \
                 50 m bins out to 1 km.",
            )
            .with_dims([Dim::T, Dim::DistBin])
            .with_breakdown(
                Dim::DistBin,
                (0..self.bins.len()).map(|i| self.bins.label(i)),
            )
            .with_source(src)
            .with_min_samples(self.min_samples)
            .with_range(0.0, 1.0)
            .not_accounting_for("attempts still in flight when the run ended")
            .not_accounting_for("receivers outside the engine's 1 km candidate range"),
        ]
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    fn on_rx(&mut self, v: &NodeRxView) {
        if v.outcome == RxFate::InFlight {
            return;
        }
        let delivered = v.outcome == RxFate::Delivered;
        self.delivery_all.observe(delivered);
        match v.dist_m.and_then(|d| self.bins.index_of(d)) {
            Some(bin) => self
                .delivery_by_bin
                .entry(bin)
                .or_default()
                .observe(delivered),
            None => self.delivery_unbinned.observe(delivered),
        }
        if !delivered {
            return;
        }
        let (Some(tx), Some(d), Some(g)) = (v.tx, v.t_delivered, v.t_generated) else {
            return;
        };
        let key = (v.rx, tx);
        let heard = self.last_heard.entry(key).or_insert(d);
        *heard = (*heard).max(d);
        match self.links.get(&key).copied() {
            Some(prev) if g > prev.generated && d >= prev.delivered => {
                if d.saturating_sub(prev.delivered) <= AOI_RESTART.as_nanos() {
                    // The area of one trapezoid of the sawtooth, in ns²: the age runs from
                    // d(k−1) − g(k−1) to d(k) − g(k−1) over an interval of d(k) − d(k−1).
                    let dt = u128::from(d - prev.delivered);
                    let a0 = u128::from(prev.delivered.saturating_sub(prev.generated));
                    let a1 = u128::from(d.saturating_sub(prev.generated));
                    self.aoi_area += dt * (a0 + a1) / 2;
                    self.aoi_time += dt;
                    self.aoi_updates += 1;
                    self.aoi_peak_ms
                        .observe((d.saturating_sub(prev.generated) as f64) / 1e6);
                }
                self.links.insert(
                    key,
                    Link {
                        delivered: d,
                        generated: g,
                    },
                );
            }
            // An older message than the one already held changes nothing about the
            // receiver's freshness.
            Some(_) => {}
            None => {
                self.links.insert(
                    key,
                    Link {
                        delivered: d,
                        generated: g,
                    },
                );
            }
        }
    }

    fn on_kinematics(&mut self, v: &GtKinematicsView) {
        // A record from a new instant closes the previous one: every vehicle's position at
        // that instant is in, so its neighbourhoods can be sampled.
        if self.frame_t != Some(v.t) {
            if let Some(prev) = self.frame_t {
                self.sample_nar(prev);
            }
            self.frame_t = Some(v.t);
        }
        if let Some(node) = v.node {
            self.positions.insert(node, (v.t, v.x_m, v.y_m));
        }
    }

    /// Samples `nar` at one instant into the window's accumulators, once per instant.
    ///
    /// Sampled at every published instant (every mobility step) rather than once per
    /// window, so a sparse run's few neighbour pairs are observed ten times a second rather
    /// than once. The observations of one pair are correlated, which is why each pair is
    /// also recorded as a *cluster*: the interval is computed on the number of distinct
    /// pairs ([`Proportion::estimate_clustered`]), not on the number of observations.
    fn sample_nar(&mut self, at: SimTime) {
        if self.nar_sampled_at == Some(at) {
            return;
        }
        self.nar_sampled_at = Some(at);
        for (k, (_, p, pairs)) in self.nar_counts(at).into_iter().enumerate() {
            self.nar_window[k].merge(p);
            self.nar_pairs[k].extend(pairs);
        }
    }

    /// `nar` at `at` for every radius: (heard, neighbours) pair counts, and the pairs.
    fn nar_counts(&self, at: SimTime) -> Vec<(u32, Proportion, Vec<(NodeId, NodeId)>)> {
        let fresh: Vec<(NodeId, f64, f64)> = self
            .positions
            .iter()
            .filter(|(_, (t, _, _))| at.saturating_sub(*t) <= POSITION_STALE.as_nanos())
            .map(|(n, (_, x, y))| (*n, *x, *y))
            .collect();
        let largest = f64::from(NAR_RADII_M.iter().copied().max().unwrap_or(300));
        // A uniform grid of the largest radius: a neighbour query touches at most nine
        // cells. Keys are integer cell indices, so the grid is a `BTreeMap` walked in order.
        let cell = |x: f64, y: f64| -> (i64, i64) {
            ((x / largest).floor() as i64, (y / largest).floor() as i64)
        };
        let mut grid: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
        for (i, (_, x, y)) in fresh.iter().enumerate() {
            grid.entry(cell(*x, *y)).or_default().push(i);
        }
        let horizon = NAR_HORIZON.as_nanos();
        let mut out: Vec<(u32, Proportion, Vec<(NodeId, NodeId)>)> = NAR_RADII_M
            .iter()
            .map(|&r| (r, Proportion::new(), Vec::new()))
            .collect();
        for (i, (rx, x, y)) in fresh.iter().enumerate() {
            let (cx, cy) = cell(*x, *y);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let Some(members) = grid.get(&(cx + dx, cy + dy)) else {
                        continue;
                    };
                    for &j in members {
                        if j == i {
                            continue;
                        }
                        let (tx, xj, yj) = fresh[j];
                        let (ex, ey) = (xj - x, yj - y);
                        let d2 = ex * ex + ey * ey;
                        // Heard within the horizon before `at`. An instant is sampled when
                        // the next instant's first record arrives, so a delivery up to one
                        // mobility step after `at` may already be in, and it counts: the
                        // latest delivery is all a link keeps, and refusing it would forget
                        // every earlier one. The bias is at most one step of a 1 s horizon.
                        let heard = self
                            .last_heard
                            .get(&(*rx, tx))
                            .is_some_and(|&t| t >= at.saturating_sub(horizon));
                        for (r, p, pairs) in &mut out {
                            let r = f64::from(*r);
                            if d2 <= r * r {
                                p.observe(heard);
                                pairs.push((*rx, tx));
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

impl Model for AwarenessProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for AwarenessProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![NodeRxView::channel_name(), GtKinematicsView::channel_name()]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        self.on_decoded(&Decoded::new(ev));
    }

    fn on_decoded(&mut self, ev: &Decoded<'_>) {
        match ev.channel() {
            NodeRxView::CHANNEL => ev.with(|v: Option<&NodeRxView>| match v {
                Some(v) => self.on_rx(v),
                None => self.rejected += 1,
            }),
            GtKinematicsView::CHANNEL => ev.with(|v: Option<&GtKinematicsView>| match v {
                Some(v) => self.on_kinematics(v),
                None => self.rejected += 1,
            }),
            _ => {}
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();

        // --- aoi and aoi_peak ------------------------------------------------------------
        let (area, time, n) = (
            core::mem::take(&mut self.aoi_area),
            core::mem::take(&mut self.aoi_time),
            core::mem::take(&mut self.aoi_updates),
        );
        // ns² / ns = ns; to ms. Both sums are exact integers until this one division.
        let (num_ms_ns, den_ns) = ((area as f64) / 1e6, time as f64);
        out.push(MetricSample::new(
            &self.def("aoi"),
            at,
            Dims::new(),
            SampleValue::Ratio(ratio_of_sums(num_ms_ns, den_ns, n, 1)),
        ));
        let peak = core::mem::replace(&mut self.aoi_peak_ms, Distribution::new());
        out.push(MetricSample::new(
            &self.def("aoi_peak"),
            at,
            Dims::new(),
            SampleValue::Distribution(peak.summary(self.min_samples)),
        ));

        // --- nar -------------------------------------------------------------------------
        // The instant being assembled belongs to this window if it is not past it.
        if let Some(t) = self.frame_t
            && t <= at
        {
            self.sample_nar(t);
        }
        let nar = self.def("nar");
        let window = core::mem::replace(&mut self.nar_window, [Proportion::new(); 2]);
        let pairs = core::mem::take(&mut self.nar_pairs);
        for ((r, p), clusters) in NAR_RADII_M.iter().zip(window).zip(pairs) {
            let mut dims = Dims::new();
            dims.insert(Dim::Radius, DimValue::label(format!("{r}m")));
            out.push(MetricSample::new(
                &nar,
                at,
                dims,
                SampleValue::Ratio(p.estimate_clustered(
                    clusters.len() as u64,
                    NAR_MIN_PAIRS,
                    self.level,
                )),
            ));
        }

        // --- delivery_ratio --------------------------------------------------------------
        let dr = self.def("delivery_ratio");
        let all = core::mem::replace(&mut self.delivery_all, Proportion::new());
        out.push(MetricSample::new(
            &dr,
            at,
            Dims::new(),
            SampleValue::Ratio(all.estimate(self.min_samples, self.level)),
        ));
        for (bin, p) in core::mem::take(&mut self.delivery_by_bin) {
            let mut dims = Dims::new();
            dims.insert(Dim::DistBin, DimValue::label(self.bins.label(bin)));
            out.push(MetricSample::new(
                &dr,
                at,
                dims,
                SampleValue::Ratio(p.estimate(self.min_samples, self.level)),
            ));
        }
        let unbinned = core::mem::replace(&mut self.delivery_unbinned, Proportion::new());
        if unbinned.trials() > 0 {
            let mut dims = Dims::new();
            dims.insert(Dim::DistBin, DimValue::label(crate::comms::UNBINNED));
            out.push(MetricSample::new(
                &dr,
                at,
                dims,
                SampleValue::Ratio(unbinned.estimate(self.min_samples, self.level)),
            ));
        }

        // --- bookkeeping -----------------------------------------------------------------
        // Bounded state: a link or a sighting older than the restart horizon can no longer
        // affect any metric, so it is dropped rather than kept for the life of the run.
        let keep = at.saturating_sub(AOI_RESTART.as_nanos());
        self.links.retain(|_, l| l.delivered >= keep);
        self.last_heard.retain(|_, t| *t >= keep);
        self.positions
            .retain(|_, (t, _, _)| at.saturating_sub(*t) <= POSITION_STALE.as_nanos());
        self.window_start = at;
        out
    }

    fn rejected(&self) -> u64 {
        self.rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ctx::OwnedRecord;
    use v2xw_core::ids::ActorId;
    use v2xw_core::time::NS_PER_MS;

    fn delivered(rx: u32, tx: u32, g0: SimTime, at: SimTime, dist: f64) -> OwnedRecord {
        let json = serde_json::json!({
            "t": at, "rx": rx, "tx": tx, "msg": g0, "msg_type": "bsm",
            "outcome": "delivered", "dist_m": dist,
            "t_generated": g0, "t_delivered": at,
        });
        OwnedRecord {
            channel: "node.rx",
            visibility: Visibility::NodeAndGt,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    fn lost(rx: u32, tx: u32, at: SimTime, dist: f64) -> OwnedRecord {
        let json = serde_json::json!({
            "t": at, "rx": rx, "tx": tx, "outcome": "lost", "cause": "fading", "dist_m": dist,
        });
        OwnedRecord {
            channel: "node.rx",
            visibility: Visibility::NodeAndGt,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    fn kin(node: u32, t: SimTime, x: f64) -> OwnedRecord {
        let v = GtKinematicsView {
            t,
            actor: ActorId::new(node),
            x_m: x,
            y_m: 0.0,
            z_m: None,
            speed_mps: 10.0,
            acc_mps2: None,
            heading_rad: None,
            lane: None,
            lane_pos_m: None,
            class: None,
            node: Some(NodeId::new(node)),
        };
        OwnedRecord {
            channel: "gt.kinematics",
            visibility: Visibility::Gt,
            json: serde_json::to_vec(&v).unwrap(),
        }
    }

    fn point(s: &[MetricSample], name: &str, dims: &[(Dim, &str)]) -> Option<f64> {
        s.iter()
            .find(|x| {
                x.metric == name
                    && x.dims.len() == dims.len()
                    && dims
                        .iter()
                        .all(|(d, v)| x.dims.get(d) == Some(&DimValue::label(*v)))
            })
            .and_then(|x| x.value.point())
    }

    /// A link updated every 100 ms with a 2 ms delay has a sawtooth from 2 ms to 102 ms,
    /// so its time-average age is 52 ms and every peak is 102 ms.
    #[test]
    fn aoi_of_a_periodic_link_is_half_the_period_plus_the_delay() {
        let mut p = AwarenessProvider::new(0).with_min_samples(1);
        for k in 0..11u64 {
            let g0 = k * 100 * NS_PER_MS;
            p.on_event(&delivered(1, 2, g0, g0 + 2 * NS_PER_MS, 30.0));
        }
        let s = p.flush(2_000 * NS_PER_MS);
        assert_eq!(point(&s, "aoi", &[]), Some(52.0));
        let peak = s.iter().find(|x| x.metric == "aoi_peak").unwrap();
        assert_eq!(peak.value.point(), Some(102.0));
        assert_eq!(peak.value.n(), 10, "the first message has no predecessor");
    }

    #[test]
    fn nar_counts_heard_neighbours_within_the_radius() {
        let mut p = AwarenessProvider::new(0).with_min_samples(1);
        let t = 5_000 * NS_PER_MS;
        // Node 1 at 0 m; 2 at 50 m (heard); 3 at 90 m (never heard); 4 at 250 m (heard
        // 1.5 s ago, too long); 5 at 1 km (not a neighbour at any radius).
        for (n, x) in [(1, 0.0), (2, 50.0), (3, 90.0), (4, 250.0), (5, 1000.0)] {
            p.on_event(&kin(n, t, x));
        }
        p.on_event(&delivered(
            1,
            2,
            t - 300 * NS_PER_MS,
            t - 200 * NS_PER_MS,
            50.0,
        ));
        p.on_event(&delivered(
            1,
            4,
            t - 1_600 * NS_PER_MS,
            t - 1_500 * NS_PER_MS,
            250.0,
        ));
        let s = p.flush(t);
        let nar = |r: &str| {
            s.iter()
                .find(|x| {
                    x.metric == "nar" && x.dims.get(&Dim::Radius) == Some(&DimValue::label(r))
                })
                .unwrap()
                .value
                .clone()
        };
        // Pairs within 100 m: 1-2, 1-3, 2-1, 2-3, 3-1, 3-2 — six, one of them heard (1←2).
        assert_eq!(nar("100m").n(), 6);
        assert!((nar("100m").point().unwrap() - 1.0 / 6.0).abs() < 1e-3);
        // Within 300 m: those six plus 1-4, 4-1, 2-4, 4-2, 3-4, 4-3; still one heard.
        assert_eq!(nar("300m").n(), 12);
    }

    /// Two vehicles 50 m apart for one second, sampled at every 100 ms step; vehicle 1
    /// hears vehicle 2 throughout and vehicle 2 never hears vehicle 1. That is two pairs —
    /// far below the thirty a proportion normally needs — and `nar` still reports: 0.5, with
    /// the Wilson interval of two independent observations, not of the twenty samples.
    #[test]
    fn a_sparse_run_reports_nar_with_the_interval_of_its_few_pairs() {
        let mut p = AwarenessProvider::new(0); // the default threshold of 30 samples
        for k in 0..10u64 {
            let t = k * 100 * NS_PER_MS;
            p.on_event(&kin(1, t, 0.0));
            p.on_event(&kin(2, t, 50.0));
            p.on_event(&delivered(1, 2, t, t + NS_PER_MS, 50.0));
        }
        let s = p.flush(1_000 * NS_PER_MS);
        let nar = s
            .iter()
            .find(|x| x.metric == "nar" && x.dims.get(&Dim::Radius) == Some(&DimValue::label("100m")))
            .unwrap();
        assert!(!nar.value.is_insufficient(), "{:?}", nar.value);
        assert_eq!(nar.value.point(), Some(0.5));
        // Twenty pair observations (two pairs at ten instants) behind the point …
        assert_eq!(nar.value.n(), 20);
        // … and the interval of two pairs: Wilson(0.5, n = 2) at 95 % is (0.0945, 0.9055).
        let SampleValue::Ratio(r) = &nar.value else {
            panic!("nar is a ratio")
        };
        let (lo, hi) = r.interval().expect("an estimate carries its interval");
        assert!((lo - 0.0945).abs() < 1e-3, "{lo}");
        assert!((hi - 0.9055).abs() < 1e-3, "{hi}");
        // Not the falsely tight interval of twenty independent trials, (0.299, 0.701).
        assert!(lo < 0.2 && hi > 0.8);
    }

    #[test]
    fn a_window_with_no_neighbour_pair_is_insufficient_not_zero() {
        let mut p = AwarenessProvider::new(0);
        p.on_event(&kin(1, 0, 0.0));
        p.on_event(&kin(2, 0, 900.0));
        let s = p.flush(1_000 * NS_PER_MS);
        for x in s.iter().filter(|x| x.metric == "nar") {
            assert!(x.value.is_insufficient(), "{:?}", x.value);
        }
    }

    #[test]
    fn delivery_ratio_by_distance_counts_losses_above_the_phy() {
        let mut p = AwarenessProvider::new(0).with_min_samples(1);
        p.on_event(&delivered(1, 2, 0, NS_PER_MS, 20.0));
        p.on_event(&lost(1, 3, NS_PER_MS, 30.0));
        p.on_event(&lost(1, 3, NS_PER_MS, 480.0));
        let s = p.flush(NS_PER_MS * 10);
        assert!((point(&s, "delivery_ratio", &[]).unwrap() - 1.0 / 3.0).abs() < 1e-3);
        assert_eq!(
            point(&s, "delivery_ratio", &[(Dim::DistBin, "0-50")]),
            Some(0.5)
        );
        assert_eq!(
            point(&s, "delivery_ratio", &[(Dim::DistBin, "450-500")]),
            Some(0.0)
        );
    }
}
