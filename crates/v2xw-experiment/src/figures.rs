//! The figure presets of 08-measurement-and-data.md §7: a results table in, a plot out,
//! with no hand-written analysis code in between.
//!
//! §7 traces each canonical research question from scenario to figure and ends every trace
//! with "figure preset `rqN`". This module is those presets. A preset is a *declaration* —
//! which metric goes on which axis of which panel, and what the panel is for — and
//! [`render`] turns it plus a [`ResultsTable`] into Plotly figure JSON, which is the one
//! figure format this project has committed to: 08 §3 says the Python API and the Studio
//! render "the same figure JSON so a figure built in the UI reproduces in a notebook".
//!
//! There is **no `v2xw figure` subcommand**. The presets are a library; wiring them to
//! the command-line tool is owed work in `v2xw-cli`, which already has
//! `v2xw experiment run|status|resume`. Until then three lines do it:
//!
//! ```no_run
//! use v2xw_experiment::figures::{preset, render};
//! # let table: v2xw_experiment::ResultsTable = unimplemented!();
//! let spec = preset("rq-pdr-distance").expect("a known preset");
//! let figures = render(&spec, &table);
//! for file in figures.write(std::path::Path::new("runs/sweep/figures"))? {
//!     println!("{file}");
//! }
//! # Ok::<(), v2xw_experiment::ExperimentError>(())
//! ```
//!
//! # A panel whose metric does not exist says so, on the figure
//!
//! Most of §7's panels are plottable today and several are not, because the metric behind
//! them has no provider yet (`t95_enforce`, `enforcement_fraction`, `residual_harm`,
//! `frag_reassembly_fail`, the whole privacy family of 07-threats §6). A preset that
//! quietly dropped those panels would let a reader believe the figure is the whole answer.
//!
//! So every panel declares its state:
//!
//! * **[`PanelSpec::blocked`] set** — the metric is known not to exist in this build, and
//!   the panel renders as a placeholder carrying the reason and what is owed. It is
//!   counted in [`FigureSet::blocked`].
//! * **blocked not set and no rows found** — the metric exists but this run produced
//!   nothing for it (wrong scenario, too short, provider not installed). The panel renders
//!   as a placeholder saying *that*, which is a different diagnosis, and is counted in
//!   [`FigureSet::empty`].
//! * **rows found** — the panel is a plot.
//!
//! The two placeholders read differently on purpose. "This engine cannot measure it" and
//! "this run did not measure it" send a reader to different places.
//!
//! # Determinism
//!
//! * Every group is a [`BTreeMap`], so the trace order is the series-value order and not
//!   the row order.
//! * Points are sorted by their x value — numerically when the label parses as a number,
//!   lexicographically otherwise, and numeric before non-numeric so an `unbinned` bucket
//!   lands after `400-425` rather than between `25-50` and `50-75`.
//! * **Every float is quantised at the writer** onto [`Q_FIGURE`], which is
//!   `v2xw_record::grid::Q_METRIC_VALUE` — the same grid the results table's own float
//!   columns declare (build decision D9). A figure is an exported artefact.
//! * No wall clock is read and nothing is drawn, so the same table produces byte-identical
//!   figure JSON.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::aggregate::CellAggregate;
use crate::error::{ExperimentError, Result};
use crate::table::ResultsTable;

/// The schema id a figure set carries.
pub const FIGURES_SCHEMA: &str = "v2xw/experiment-figures/1";

/// The grid every float in a figure sits on: `v2xw_record::grid::Q_METRIC_VALUE`, 1e-6.
///
/// The finest grid build decision D9 lists, which is the safe direction for a file whose
/// values come from metrics with different declared grids: a value already on a coarser
/// grid is bit-for-bit unchanged by a finer one, while declaring a coarse grid here would
/// destroy precision a metric's own contract promised.
pub const Q_FIGURE: f64 = v2xw_record::grid::Q_METRIC_VALUE;

/// What a panel's axis reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Axis {
    /// A swept parameter, by its dotted scenario path — the sweep's own axis.
    Param(&'static str),
    /// A metric dimension, by the name `v2xw_metrics::Dim` serialises to: `dist_bin`,
    /// `stage`, `cause`, `detector`, `primitive`, `level`, `node`, `channel`, `region`.
    Dim(&'static str),
}

impl Axis {
    /// The axis label a figure carries when the preset gives none.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Axis::Param(path) | Axis::Dim(path) => path,
        }
    }

    /// This axis's value on one row, or `None` when the row does not carry it.
    ///
    /// For a [`Axis::Param`] the value is the cell's swept value; for a [`Axis::Dim`] it
    /// is the dimension's label out of the row's `dims` document.
    #[must_use]
    pub fn value_of(self, row: &CellAggregate, dims: &BTreeMap<String, String>) -> Option<String> {
        match self {
            Axis::Param(path) => row.cell.values.get(path).map(render_value),
            Axis::Dim(name) => dims.get(name).cloned(),
        }
    }
}

/// One panel of a figure.
// `Serialize` only, no `Deserialize`: every field is a `&'static str`, and serde cannot
// deserialise into one. A preset is code, not data — it is read from this module and never
// parsed back — so the asymmetry costs nothing and saying why keeps somebody from "fixing"
// it into a compile error.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelSpec {
    /// The panel's id, and the stem of the file it is written to.
    pub id: &'static str,
    /// The panel's title.
    pub title: &'static str,
    /// The metric whose rows it plots, by `MetricDef::name`.
    pub metric: &'static str,
    /// What goes on the x axis.
    pub x: Axis,
    /// What separates the traces, if anything.
    pub series: Option<Axis>,
    /// The y axis's label, including its unit.
    pub y_label: &'static str,
    /// What the panel is for, and how to read it. Rendered into the figure's metadata and
    /// into the placeholder when there is nothing to plot.
    pub note: &'static str,
    /// Set when this build is **known** not to be able to measure the metric: the reason,
    /// and what is owed.
    ///
    /// A blocked panel is never plotted, even if rows happen to appear, because a metric
    /// name that collides with a blocked one would otherwise produce a figure labelled as
    /// something it is not.
    pub blocked: Option<&'static str>,
}

impl PanelSpec {
    /// A plottable panel.
    #[must_use]
    pub fn new(
        id: &'static str,
        title: &'static str,
        metric: &'static str,
        x: Axis,
        y_label: &'static str,
        note: &'static str,
    ) -> Self {
        Self {
            id,
            title,
            metric,
            x,
            series: None,
            y_label,
            note,
            blocked: None,
        }
    }

    /// The same panel, with its traces separated by `series`.
    #[must_use]
    pub fn by(mut self, series: Axis) -> Self {
        self.series = Some(series);
        self
    }

    /// The same panel, declared unmeasurable in this build.
    #[must_use]
    pub fn blocked_by(mut self, reason: &'static str) -> Self {
        self.blocked = Some(reason);
        self
    }
}

/// A named set of panels: the figure for one research question.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FigurePreset {
    /// The preset's canonical id, as 08-measurement-and-data.md §7 names it.
    pub id: &'static str,
    /// Other names the same preset answers to — the study file's own name, so a caller
    /// can be given either. There is no `v2xw figure` subcommand yet: [`preset`] is the
    /// entry point and wiring it to the command-line tool is owed work in `v2xw-cli`.
    pub aliases: &'static [&'static str],
    /// A one-line title.
    pub title: &'static str,
    /// The question the figure answers, as the design document asks it.
    pub question: &'static str,
    /// The scenario this preset expects to have been run.
    pub scenario: &'static str,
    /// The panels, in reading order.
    pub panels: Vec<PanelSpec>,
}

impl FigurePreset {
    /// True if `name` is this preset's id or one of its aliases.
    #[must_use]
    pub fn answers_to(&self, name: &str) -> bool {
        self.id == name || self.aliases.contains(&name)
    }
}

/// Every preset's canonical id, in reading order.
pub const PRESET_IDS: &[&str] = &["rq1", "rq3", "rq4", "rq5", "rq6"];

/// The preset called `name`, by id or alias.
#[must_use]
pub fn preset(name: &str) -> Option<FigurePreset> {
    all_presets().into_iter().find(|p| p.answers_to(name))
}

/// Every preset, in [`PRESET_IDS`] order.
#[must_use]
pub fn all_presets() -> Vec<FigurePreset> {
    vec![rq1(), rq3(), rq4(), rq5(), rq6()]
}

/// RQ1 — the dense-downtown air-interface and verification picture
/// (08-measurement-and-data.md §7, RQ1 item 5).
///
/// §7 asks for "PDR vs distance per protocol per density; CBR vs vehicle count;
/// verification queue p95 vs vehicle count; the density at which
/// `frag_reassembly_fail` exceeds 5 % is read off the third panel". Three of those four
/// are plottable; the fragmentation one is not, and says so.
#[must_use]
pub fn rq1() -> FigurePreset {
    FigurePreset {
        id: "rq1",
        aliases: &["rq-pdr-distance", "rq-density-congestion"],
        title: "Air interface and verification load against distance and density",
        question: "Where does delivery fail as distance and offered load rise, and which \
                   of the channel and the receiver runs out first?",
        scenario: "scenarios/pdr-vs-distance.yaml, scenarios/density-sweep-congestion.yaml",
        panels: vec![
            PanelSpec::new(
                "pdr-vs-distance",
                "Packet delivery ratio against distance",
                "pdr",
                Axis::Dim("dist_bin"),
                "pdr (ratio)",
                "The 25 m bins are the metric provider's own. A bin whose pooled trial \
                 count is below the metric's `min_samples` reports insufficient and is \
                 absent from this panel rather than plotted as a point estimate, so a \
                 gap in the curve is a thin bin and not a zero.",
            )
            .by(Axis::Param("actors.vehicles.demand.rate_veh_per_h")),
            PanelSpec::new(
                "pdr-by-cause",
                "Where the losses went",
                "pdr_by_cause",
                Axis::Dim("cause"),
                "share of losses (ratio)",
                "The PHY reports at most one loss cause per frame (invariant I-R3), so \
                 these shares sum to one. At the `medium` PHY tier there are no \
                 interferers in the SINR sum, so no collision cause can appear — an \
                 empty `Collision` bar here is a property of the tier and not of the \
                 channel.",
            ),
            PanelSpec::new(
                "cbr-vs-load",
                "Channel busy ratio against offered demand",
                "cbr",
                Axis::Param("actors.vehicles.demand.rate_veh_per_h"),
                "cbr (ratio)",
                "The x axis is the *offered* demand rate, not a realised density: there \
                 is no run-level density metric (`density` is per lane, keyed by \
                 `region`), so the realised traffic state is read off the traffic panels \
                 and reported beside this one.",
            ),
            PanelSpec::new(
                "verify-queue-vs-load",
                "Verification queue depth against offered demand",
                "verify_queue_depth",
                Axis::Param("actors.vehicles.demand.rate_veh_per_h"),
                "queue depth (messages)",
                "The receiver's half of congestion. A node that hears four hundred signed \
                 messages a second does not verify four hundred a second, and this is \
                 where that shows up first — before the delivery curve moves at all.",
            ),
            PanelSpec::new(
                "unverified-vs-load",
                "Share delivered without verification",
                "unverified_ratio",
                Axis::Param("actors.vehicles.demand.rate_veh_per_h"),
                "unverified share (ratio)",
                "What the verification policy gave up on. Under `verify-all` this rising \
                 is the queue overflowing; under `prioritized` it is the policy working \
                 as designed, and the two are indistinguishable from this panel alone.",
            ),
            PanelSpec::new(
                "frag-reassembly-fail",
                "Fragment reassembly failures against offered demand",
                "frag_reassembly_fail",
                Axis::Param("actors.vehicles.demand.rate_veh_per_h"),
                "reassembly failure (ratio)",
                "§7's third RQ1 panel: the density at which reassembly failure exceeds \
                 5 % is the headline number of the fragmentation study.",
            )
            .blocked_by(
                "`frag_reassembly_fail` and `frag_loss_amplification` have no metric \
                 provider in this build: `v2xw-metrics` registers comms, security, \
                 detection, safety and runtime, and none of them reads `net.frag`. The \
                 fragmenter itself is Phase 3 scope. Owed: a provider over `net.frag` \
                 plus a `msg_type` dimension.",
            ),
        ],
    }
}

/// RQ3 — revocation scaling (08-measurement-and-data.md §7, RQ3 item 5).
///
/// §7 asks for "CRL size vs revoked count; download time by path (RSU vs cellular); t95
/// vs traffic; residual harm vs cadence". The list size is plottable and the stage
/// decomposition — which §7 does not ask for and which is the more informative figure — is
/// plottable too. The other three are not, each for its own reason.
#[must_use]
pub fn rq3() -> FigurePreset {
    FigurePreset {
        id: "rq3",
        aliases: &["rq-revocation-latency"],
        title: "Revocation latency, decomposed, and the cost of the list",
        question: "How long does each stage of a revocation take, how big does the list \
                   get, and how much harm does the device do in the meantime?",
        scenario: "scenarios/revocation-latency.yaml",
        panels: vec![
            PanelSpec::new(
                "latency-by-stage",
                "Revocation latency by protocol stage",
                "revocation_latency_stage",
                Axis::Dim("stage"),
                "stage duration (s)",
                "The decomposition of 05-protocols.md §8, which is the figure that makes \
                 a total latency interpretable: forty seconds means one thing if thirty \
                 of them are the authority's batching window and another if thirty are \
                 the CRL download. **Read the authority's stage against \
                 `ScmsParams::quick()`**, which shortens the two CAMP batching windows \
                 so a short scenario can reach a revocation at all — a cited deployment \
                 number replaced by a run-length accommodation.",
            )
            .by(Axis::Param("actors.vehicles.demand.rate_veh_per_h")),
            PanelSpec::new(
                "crl-entries-vs-attackers",
                "CRL entries against attacker fraction",
                "crl_entries",
                Axis::Param("threats.attackers[0].fraction"),
                "entries (count)",
                "The list's length, which the download time and the on-board expansion \
                 cost are both functions of: 2 SHA-256 and 2*jmax AES per entry per \
                 period.",
            )
            .by(Axis::Param("actors.vehicles.demand.rate_veh_per_h")),
            PanelSpec::new(
                "crl-bytes-vs-attackers",
                "CRL size in bytes against attacker fraction",
                "crl_bytes",
                Axis::Param("threats.attackers[0].fraction"),
                "list size (B)",
                "Bytes rather than entries, because what a vehicle downloads over a \
                 contended 5.9 GHz channel is bytes.",
            )
            .by(Axis::Param("actors.vehicles.demand.rate_veh_per_h")),
            PanelSpec::new(
                "time-to-decision",
                "Attack onset to authority decision",
                "time_to_decision",
                Axis::Param("actors.vehicles.demand.rate_veh_per_h"),
                "time to decision (s)",
                "The front half of the path: detection, reporting, transport, resolution \
                 and the decision. The stage panel is the back half.",
            )
            .by(Axis::Param("threats.attackers[0].fraction")),
            PanelSpec::new(
                "t95-enforce",
                "Time until 95 % of nodes enforce",
                "t95_enforce",
                Axis::Param("actors.vehicles.demand.rate_veh_per_h"),
                "t95 (s)",
                "§7's third RQ3 panel, and the number a deployment actually cares about.",
            )
            .blocked_by(
                "`t95_enforce` and `enforcement_fraction` are defined in \
                 08-measurement-and-data.md §2.3 and have no provider. Owed: a provider \
                 over `proto.revocation` joined against each node's CRL-store state, \
                 which means the store has to emit an installation record.",
            ),
            PanelSpec::new(
                "residual-harm",
                "Messages from a revoked device still accepted",
                "residual_harm",
                Axis::Param("actors.vehicles.demand.rate_veh_per_h"),
                "accepted messages (count)",
                "§2.3's sharpest number: what the revocation did not prevent, counted \
                 after the decision, after the issuance and after the publication.",
            )
            .blocked_by(
                "No provider. It is derivable after the fact from a recording — join \
                 `phy.rx` against the `proto.revocation` stage timestamps — so the route \
                 today is `v2xw run --record` on a materialised cell rather than a \
                 metric. Owed: that join as a provider, which needs the revoked device's \
                 pseudonym set, which is ground truth and therefore a `GT`-tagged metric.",
            ),
            PanelSpec::new(
                "crl-download-by-path",
                "CRL download time by distribution path",
                "crl_download_time",
                Axis::Param("actors.vehicles.demand.rate_veh_per_h"),
                "download time (s)",
                "§7's second RQ3 panel: the roadside broadcast against the cellular path.",
            )
            .blocked_by(
                "Two things are missing, not one. There is no `crl_download_time` metric, \
                 and there is no cellular path to compare against: the Uu models exist in \
                 `v2xw-radio` and `v2xw_engine::wiring` builds no Uu link, so a \
                 `cellular.uu_model` sweep selects nothing.",
            ),
        ],
    }
}

/// RQ4 — an authenticated attacker's detection and propagation latency
/// (08-measurement-and-data.md §7, RQ4 item 5).
///
/// §7 asks for "stacked latency by stage vs RSU density, one panel per coverage level,
/// both protocols". The stage axis is plottable; the RSU-density and coverage axes are not
/// sweeps this engine can run, so the panels that need them are blocked and the ones that
/// remain are the detection and accusation metrics, which are the point of the question.
#[must_use]
pub fn rq4() -> FigurePreset {
    FigurePreset {
        id: "rq4",
        aliases: &["rq-insider-detection", "rq-pseudonym-privacy"],
        title: "Detecting an authenticated attacker, and what it costs to be wrong",
        question: "How long does it take to detect, decide about and propagate against a \
                   device holding valid credentials, and who else gets accused?",
        scenario: "scenarios/revocation-latency.yaml, scenarios/pseudonym-privacy.yaml",
        panels: vec![
            PanelSpec::new(
                "time-to-detect",
                "Attack onset to first correct report at the authority",
                "time_to_detect",
                Axis::Param("security.pseudonym_change.period_s"),
                "time to detect (s)",
                "Read this beside the recall panel, always. An attacker never reported \
                 contributes no sample at all, so a *falling* detection time can mean \
                 the hard cases stopped being reported rather than that detection \
                 improved. The metric's own definition says so.",
            ),
            PanelSpec::new(
                "det-recall",
                "Detection recall",
                "det_recall",
                Axis::Param("security.pseudonym_change.period_s"),
                "recall (ratio)",
                "Two series: `level=report` is \"at least one report about this subject \
                 reached the authority\", `level=vehicle` is \"the authority decided to \
                 revoke it\". The gap between them is the authority's pipeline, not the \
                 detectors'.",
            )
            .by(Axis::Dim("level")),
            PanelSpec::new(
                "det-precision",
                "Detection precision",
                "det_precision",
                Axis::Param("security.pseudonym_change.period_s"),
                "precision (ratio)",
                "Both summaries hide the base rate, in opposite directions. With 2 % \
                 attackers a 1 % false-positive rate still produces more false than true \
                 positives, which is what the confusion-matrix counts show and what a \
                 ratio cannot.",
            )
            .by(Axis::Dim("level")),
            PanelSpec::new(
                "false-accusations",
                "Benign vehicles accused",
                "false_accusations",
                Axis::Param("security.pseudonym_change.period_s"),
                "accused vehicles (count)",
                "A count, not a rate, because the harm is per vehicle. A benign subject \
                 reported and then cleared is still an accusation and is still counted.",
            ),
            PanelSpec::new(
                "linkability-vs-period",
                "Pseudonym linkability against the change period",
                "linkability_rate",
                Axis::Param("security.pseudonym_change.period_s"),
                "correctly linked changes (ratio)",
                "The other half of the pseudonym question: the cost panels say what a \
                 short period costs, and this would say what it buys. Read it with \
                 `tracking_duration`, because a policy that halves the linkability rate \
                 and leaves the mean tracked duration unchanged has bought nothing a \
                 driver would notice.",
            )
            .blocked_by(
                "The observer exists and nothing installs it. `v2xw_threat::privacy` is \
                 the passive adversary of 07-threats §6 — a constant-velocity tracker by \
                 the Wiedersheim method, emitting a link claim per pseudonym change and \
                 the claims it declines to make — and `v2xw_metrics::register_all` \
                 installs five providers, none of which reads a privacy record. So \
                 `linkability_rate`, `anonymity_set_size`, `degree_of_anonymity` and \
                 `tracking_duration` are not in the metric catalog. Owed: a scenario key \
                 that places observers, a provider over their records, and the \
                 ground-truth digest-to-actor declaration the correctness join needs \
                 (the shape `DetectionProvider::declare_subject` already has).",
            ),
            PanelSpec::new(
                "stage-latency-by-rsu",
                "Stacked latency by stage against roadside-unit density",
                "revocation_latency_stage",
                Axis::Param("actors.rsus.count"),
                "stage duration (s)",
                "§7's RQ4 figure as written: one panel per cellular coverage level, both \
                 credential protocols, stacked by stage.",
            )
            .blocked_by(
                "Three axes are missing. `actors.rsus` is a list, and `experiment.sweep` \
                 replaces a value at an existing path rather than lengthening a list, so \
                 an RSU-density sweep is sibling files with `meta.base` and not a sweep. \
                 `cellular.coverage_fraction` is not a schema field and no Uu link is \
                 built. And `security.protocol` has one implementation \
                 (`protocol/scms/camp`), which `Phase2::build` refuses to deviate from by \
                 name — so \"both protocols\" needs the ETSI TS 102 941 plug-in, a Phase 4 \
                 item blocked on a codegen defect recorded in build decision D5.",
            ),
        ],
    }
}

/// RQ5 — the radio-technology comparison (08-measurement-and-data.md §7, "RQ5 is a sweep
/// on `radio.rat` with identical traffic and security").
///
/// The axis is `radio.rat`: `scenarios/rat-comparison.yaml` sweeps 802.11p, LTE-V2X
/// Mode 4 and NR-V2X Mode 2 over identical traffic and security, and its header carries
/// the measured curves and what they are compared against.
#[must_use]
pub fn rq5() -> FigurePreset {
    FigurePreset {
        id: "rq5",
        aliases: &["rq-rat-comparison"],
        title: "One radio against another, with everything else held identical",
        question: "How do DSRC, LTE-V2X and NR-V2X compare on delivery, latency and \
                   channel occupancy under identical traffic and identical security?",
        scenario: "scenarios/rat-comparison.yaml",
        panels: vec![
            PanelSpec::new(
                "pdr-vs-distance-by-rat",
                "Delivery against distance, per radio",
                "pdr",
                Axis::Dim("dist_bin"),
                "pdr (ratio)",
                "The comparison figure of RQ5: one series per radio technology, swept on \
                 `radio.rat` with the world, traffic, seed and security stack held \
                 identical.",
            )
            .by(Axis::Param("radio.rat")),
            PanelSpec::new(
                "pir-by-rat",
                "Packet inter-reception time, per radio",
                "pir",
                Axis::Param("radio.rat"),
                "inter-reception time (s)",
                "What a safety application feels, as opposed to what the link does: two \
                 radios with the same delivery ratio and different loss *clustering* are \
                 not equally useful, and this is the panel that separates them.",
            ),
            PanelSpec::new(
                "cbr-by-rat",
                "Channel busy ratio, per radio",
                "cbr",
                Axis::Param("radio.rat"),
                "cbr (ratio)",
                "Occupancy under an identical offered load. Note that the C-V2X \
                 definition is not the 802.11p one — TS 38.215 §5.1.27's sub-channel \
                 S-RSSI ratio against a busy-time fraction over a CCA threshold — so this \
                 panel compares two differently defined quantities, and the figure has to \
                 say so.",
            ),
            PanelSpec::new(
                "e2e-latency-by-rat",
                "End-to-end latency, per radio",
                "e2e_latency",
                Axis::Param("radio.rat"),
                "latency (ms)",
                "Generation to application delivery, so it carries the queueing, the air \
                 time and the verification. A sidelink radio with semi-persistent \
                 scheduling has a structurally different latency distribution from CSMA, \
                 which is the interesting comparison and the one this panel is for.",
            ),
        ],
    }
}

/// RQ6 — one, two, three vehicles (08-measurement-and-data.md §7: "RQ6 is the Phase 1-2
/// scenario ... the standards conformance checklist is the figure").
///
/// The checklist itself is `v2xw_conformance::checklist`, not a plot. What is plottable is
/// the small-scale picture the checklist is asserted against, and that is what this preset
/// draws.
#[must_use]
pub fn rq6() -> FigurePreset {
    FigurePreset {
        id: "rq6",
        aliases: &["rq-vertical-slice"],
        title: "The vertical slice, measured",
        question: "With one, two and three vehicles, does the stack do what the standards \
                   say it does?",
        scenario: "scenarios/phase1-grid.yaml, scenarios/phase2-manhattan.yaml",
        panels: vec![
            PanelSpec::new(
                "full-cert-share",
                "Share of frames carrying a full certificate",
                "full_cert_share",
                Axis::Param("security.signer_id_policy.full_cert_every_ms"),
                "full-certificate share (ratio)",
                "The J2945/1 attachment rule, measured rather than asserted: at a 1,000 \
                 ms cadence and a 10 Hz beacon this should be one frame in ten, and the \
                 conformance checklist asserts exactly that on the event log.",
            ),
            PanelSpec::new(
                "envelope-overhead",
                "Security envelope bytes per payload byte",
                "envelope_overhead",
                Axis::Param("security.signer_id_policy.full_cert_every_ms"),
                "overhead (ratio)",
                "A ratio of sums and therefore reported with its two sums and no interval: \
                 it is not a count of Bernoulli trials, so a Wilson interval on it would \
                 be a fabricated error bar. The 93-byte digest-signer overhead of \
                 04-models.md §9.1 is the number to check the denominator against.",
            ),
            PanelSpec::new(
                "verify-rate",
                "Verifications completed per second, per primitive",
                "verify_rate",
                Axis::Param("nodes.default_obu"),
                "verifications (1/s)",
                "Per hardware profile, which is the comparison that makes the profile \
                 numbers falsifiable: a Craton2 and an SoC without an HSM should differ \
                 here by the ratio their datasheets claim.",
            )
            .by(Axis::Dim("primitive")),
        ],
    }
}

/// One rendered panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Figure {
    /// The panel's id, and the stem of its file.
    pub id: String,
    /// The panel's title.
    pub title: String,
    /// The metric it plots.
    pub metric: String,
    /// How many points reached the plot.
    pub points: usize,
    /// How many traces the plot has.
    pub traces: usize,
    /// Why the panel is a placeholder rather than a plot, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// The Plotly figure document: `{data, layout}`, ready for `Plotly.newPlot` or
    /// `plotly.io.from_json`.
    pub plotly: Value,
}

impl Figure {
    /// True if this panel carries a plot rather than a placeholder.
    #[must_use]
    pub const fn is_plotted(&self) -> bool {
        self.unavailable.is_none()
    }
}

/// Every panel of one preset, rendered against one results table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FigureSet {
    /// The schema id, [`FIGURES_SCHEMA`].
    pub schema: String,
    /// The preset's id.
    pub preset: String,
    /// The preset's title.
    pub title: String,
    /// The question it answers.
    pub question: String,
    /// The experiment the table came from.
    pub experiment: String,
    /// The plan digest, which identifies the sweep. **Every figure carries it**
    /// (08-measurement-and-data.md §1: "every figure carries it in metadata"), so a plot
    /// in a paper can be traced to the runs behind it.
    pub plan_digest: String,
    /// The panels, in the preset's order.
    pub figures: Vec<Figure>,
}

impl FigureSet {
    /// How many panels are plots.
    #[must_use]
    pub fn plotted(&self) -> usize {
        self.figures.iter().filter(|f| f.is_plotted()).count()
    }

    /// The panels the preset declared unmeasurable in this build.
    #[must_use]
    pub fn blocked(&self) -> Vec<&Figure> {
        self.figures
            .iter()
            .filter(|f| {
                f.unavailable
                    .as_deref()
                    .is_some_and(|u| u.starts_with(BLOCKED_PREFIX))
            })
            .collect()
    }

    /// The panels whose metric exists and which this run produced no rows for.
    #[must_use]
    pub fn empty(&self) -> Vec<&Figure> {
        self.figures
            .iter()
            .filter(|f| {
                f.unavailable
                    .as_deref()
                    .is_some_and(|u| u.starts_with(EMPTY_PREFIX))
            })
            .collect()
    }

    /// Writes `figures.json` and one `<panel>.json` per panel into `dir`, and returns the
    /// file names in write order.
    ///
    /// # Errors
    /// [`ExperimentError::Io`] if a file cannot be written and [`ExperimentError::Json`]
    /// if a document will not serialise.
    pub fn write(&self, dir: &Path) -> Result<Vec<String>> {
        std::fs::create_dir_all(dir)
            .map_err(|e| ExperimentError::io("cannot create the figure directory", dir, e))?;
        let mut written = Vec::with_capacity(self.figures.len() + 1);
        for figure in &self.figures {
            let name = format!("{}.json", figure.id);
            let path = dir.join(&name);
            let bytes = serde_json::to_vec_pretty(&figure.plotly)
                .map_err(|e| ExperimentError::json("a figure", e))?;
            std::fs::write(&path, &bytes)
                .map_err(|e| ExperimentError::io("cannot write a figure", &path, e))?;
            written.push(name);
        }
        let index = dir.join("figures.json");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| ExperimentError::json("the figure index", e))?;
        std::fs::write(&index, &bytes)
            .map_err(|e| ExperimentError::io("cannot write the figure index", &index, e))?;
        written.push("figures.json".to_string());
        Ok(written)
    }
}

/// The prefix a blocked panel's `unavailable` text starts with.
pub const BLOCKED_PREFIX: &str = "this build cannot measure it: ";

/// The prefix an empty panel's `unavailable` text starts with.
pub const EMPTY_PREFIX: &str = "this run produced no rows for it: ";

/// Renders every panel of `spec` against `table`.
#[must_use]
pub fn render(spec: &FigurePreset, table: &ResultsTable) -> FigureSet {
    let figures = spec
        .panels
        .iter()
        .map(|panel| render_panel(spec, panel, table))
        .collect();
    FigureSet {
        schema: FIGURES_SCHEMA.to_string(),
        preset: spec.id.to_string(),
        title: spec.title.to_string(),
        question: spec.question.to_string(),
        experiment: table.experiment.clone(),
        plan_digest: table.plan_digest.clone(),
        figures,
    }
}

/// One point of one trace.
#[derive(Debug, Clone)]
struct Point {
    /// The x value as it is labelled.
    label: String,
    /// Its numeric form, when the label is a number or begins with one.
    numeric: Option<f64>,
    /// The estimate.
    y: f64,
    /// The interval's half-widths, above and below, when there is an interval.
    error: Option<(f64, f64)>,
    /// The underlying observation count, for the hover text.
    samples: u64,
    /// How many replications contributed.
    replications: u64,
}

fn render_panel(spec: &FigurePreset, panel: &PanelSpec, table: &ResultsTable) -> Figure {
    if let Some(reason) = panel.blocked {
        return placeholder(panel, format!("{BLOCKED_PREFIX}{reason}"));
    }

    // Group the rows into traces. A `BTreeMap` keyed by the series label, so the trace
    // order is the label order and not the row order.
    let mut traces: BTreeMap<String, Vec<Point>> = BTreeMap::new();
    for row in &table.rows {
        if row.metric != panel.metric {
            continue;
        }
        // A row with no estimate — every replication insufficient — is not a zero and is
        // not plotted. Dropping it is right; dropping it silently would not be, which is
        // why the panel's point count is reported beside the plot.
        let Some(mean) = row.mean.filter(|v| v.is_finite()) else {
            continue;
        };
        let dims = parse_dims(&row.dims);
        let Some(label) = panel.x.value_of(row, &dims) else {
            continue;
        };
        let series = match panel.series {
            Some(axis) => axis
                .value_of(row, &dims)
                .unwrap_or_else(|| "all".to_string()),
            None => "all".to_string(),
        };
        let error = match (row.ci_lo, row.ci_hi) {
            (Some(lo), Some(hi)) if lo.is_finite() && hi.is_finite() => {
                Some(((hi - mean).max(0.0), (mean - lo).max(0.0)))
            }
            _ => None,
        };
        traces.entry(series).or_default().push(Point {
            numeric: leading_number(&label),
            label,
            y: mean,
            error,
            samples: row.samples,
            replications: row.replications_with_value,
        });
    }

    if traces.is_empty() {
        return placeholder(
            panel,
            format!(
                "{EMPTY_PREFIX}no row of `{}` in this results table carried both an \
                 estimate and the `{}` axis. Either the scenario did not sweep it, the \
                 metric's provider was not installed, or every bin was below the \
                 metric's own `min_samples` and reported insufficient.",
                panel.metric,
                panel.x.label()
            ),
        );
    }

    let mut data = Vec::with_capacity(traces.len());
    let mut points = 0usize;
    for (series, mut ps) in traces {
        // Numeric first, in numeric order; then the non-numeric labels lexicographically.
        // So `unbinned` lands after `400-425` rather than between `25-50` and `50-75`,
        // which is where a plain string sort would put it.
        ps.sort_by(|a, b| match (a.numeric, b.numeric) {
            (Some(x), Some(y)) => x.total_cmp(&y).then_with(|| a.label.cmp(&b.label)),
            (Some(_), None) => core::cmp::Ordering::Less,
            (None, Some(_)) => core::cmp::Ordering::Greater,
            (None, None) => a.label.cmp(&b.label),
        });
        points += ps.len();
        let x: Vec<Value> = ps.iter().map(|p| Value::from(p.label.clone())).collect();
        let y: Vec<Value> = ps.iter().map(|p| number(p.y)).collect();
        let plus: Vec<Value> = ps
            .iter()
            .map(|p| number(p.error.map_or(0.0, |(hi, _)| hi)))
            .collect();
        let minus: Vec<Value> = ps
            .iter()
            .map(|p| number(p.error.map_or(0.0, |(_, lo)| lo)))
            .collect();
        let hover: Vec<Value> = ps
            .iter()
            .map(|p| {
                Value::from(format!(
                    "{} = {} over {} sample(s) in {} replication(s)",
                    panel.metric,
                    quantise(p.y),
                    p.samples,
                    p.replications
                ))
            })
            .collect();
        let has_error = ps.iter().any(|p| p.error.is_some());
        let mut trace = json!({
            "type": "scatter",
            "mode": "lines+markers",
            "name": series,
            "x": x,
            "y": y,
            "text": hover,
            "hoverinfo": "text+name",
        });
        if has_error {
            trace["error_y"] = json!({
                "type": "data",
                "symmetric": false,
                "array": plus,
                "arrayminus": minus,
                "visible": true,
            });
        }
        data.push(trace);
    }

    let plotly = json!({
        "data": data,
        "layout": {
            "title": {"text": panel.title},
            "xaxis": {"title": {"text": panel.x.label()}, "type": "category"},
            "yaxis": {"title": {"text": panel.y_label}},
            "showlegend": true,
            "template": Value::Null,
        },
        // Metadata, not decoration: 08-measurement-and-data.md §1 requires every figure to
        // carry the manifest hash. The plan digest is this layer's equivalent — it
        // identifies the sweep, and every cell's own manifest hash is in the journal.
        "v2xw": {
            "schema": FIGURES_SCHEMA,
            "preset": spec.id,
            "panel": panel.id,
            "metric": panel.metric,
            "plan_digest": table.plan_digest.as_str(),
            "experiment": table.experiment.as_str(),
            "quantum": Q_FIGURE,
            "note": panel.note,
            "error_bars": "the aggregate's interval: a Wilson score interval for a pooled \
                           proportion, mean +- z*s/sqrt(k) over replications otherwise, and \
                           absent where fewer than two replications produced a value",
        }
    });

    Figure {
        id: panel.id.to_string(),
        title: panel.title.to_string(),
        metric: panel.metric.to_string(),
        points,
        traces: plotly["data"].as_array().map_or(0, Vec::len),
        unavailable: None,
        plotly,
    }
}

/// A panel that is not a plot, with the reason on the figure itself.
fn placeholder(panel: &PanelSpec, reason: String) -> Figure {
    let plotly = json!({
        "data": [],
        "layout": {
            "title": {"text": panel.title},
            "xaxis": {"visible": false},
            "yaxis": {"visible": false},
            "annotations": [{
                "text": reason.as_str(),
                "showarrow": false,
                "align": "left",
                "xref": "paper",
                "yref": "paper",
                "x": 0.02,
                "y": 0.5,
            }],
        },
        "v2xw": {
            "schema": FIGURES_SCHEMA,
            "panel": panel.id,
            "metric": panel.metric,
            "unavailable": reason.as_str(),
            "note": panel.note,
        }
    });
    Figure {
        id: panel.id.to_string(),
        title: panel.title.to_string(),
        metric: panel.metric.to_string(),
        points: 0,
        traces: 0,
        unavailable: Some(reason),
        plotly,
    }
}

/// A row's `dims` document as a map. An unreadable one yields an empty map, which makes
/// the panel report "no rows for this axis" rather than panicking on a malformed table.
fn parse_dims(dims: &str) -> BTreeMap<String, String> {
    serde_json::from_str(dims).unwrap_or_default()
}

/// A swept value as a bare label: a string without its quotes, anything else as compact
/// JSON.
fn render_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The leading number of a label, so `"25-50"` sorts by 25 and `"1500.0"` by 1500.
///
/// Returns `None` for a label with no leading number (`"unbinned"`, `"medium"`), which the
/// sort puts after every numeric label.
fn leading_number(label: &str) -> Option<f64> {
    let bytes = label.as_bytes();
    let mut end = 0;
    // An optional sign, then digits and at most as many dots as `parse` will accept. The
    // hyphen is deliberately **not** accepted inside the run: a distance bin is spelled
    // `25-50`, so treating `-` as part of the number would make the whole label
    // unparseable and every bin sort as non-numeric. That was the first version of this
    // function and the test below is the one that caught it.
    if matches!(bytes.first(), Some(b'-' | b'+')) {
        end = 1;
    }
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    label[..end].parse::<f64>().ok()
}

/// A float on the figure's declared grid, as JSON.
fn number(x: f64) -> Value {
    serde_json::Number::from_f64(quantise(x)).map_or(Value::Null, Value::Number)
}

/// The writer-side quantisation of D9, for the one grid this module writes.
fn quantise(x: f64) -> f64 {
    v2xw_core::math::quantize_to(x, Q_FIGURE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::CiMethod;
    use crate::plan::CellKey;
    use v2xw_metrics::ConfidenceLevel;

    fn row(
        cell_index: usize,
        params: &[(&str, Value)],
        metric: &str,
        dims: &str,
        mean: Option<f64>,
    ) -> CellAggregate {
        CellAggregate {
            cell: CellKey {
                index: cell_index,
                values: params
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), v.clone()))
                    .collect(),
            },
            metric: metric.to_string(),
            unit: "ratio".to_string(),
            dims: dims.to_string(),
            agg: "ratio".to_string(),
            replications: 3,
            replications_with_value: 3,
            samples: 900,
            mean,
            ci_lo: mean.map(|m| m - 0.01),
            ci_hi: mean.map(|m| m + 0.02),
            ci_level: ConfidenceLevel::P95,
            ci_method: CiMethod::Wilson,
            stddev: Some(0.005),
            min: mean,
            max: mean,
            per_replication: vec![0.9, 0.91, 0.92],
        }
    }

    fn table(rows: Vec<CellAggregate>) -> ResultsTable {
        ResultsTable {
            schema: crate::table::RESULTS_SCHEMA.to_string(),
            experiment: "test-sweep".to_string(),
            plan_digest: "deadbeef".to_string(),
            axes: vec!["actors.vehicles.demand.rate_veh_per_h".to_string()],
            rows,
        }
    }

    #[test]
    fn every_preset_id_resolves_and_is_unique() {
        let all = all_presets();
        assert_eq!(all.len(), PRESET_IDS.len());
        for id in PRESET_IDS {
            assert!(preset(id).is_some(), "{id} does not resolve");
        }
        // No alias may collide with another preset's id or alias, or `preset()` would
        // return whichever happened to be first.
        let mut names: Vec<&str> = Vec::new();
        for p in &all {
            names.push(p.id);
            names.extend(p.aliases.iter().copied());
        }
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "two presets answer to one name");
    }

    #[test]
    fn every_panel_id_is_unique_within_its_preset() {
        // Panel ids are file stems, so a duplicate would overwrite a figure.
        for p in all_presets() {
            let mut ids: Vec<&str> = p.panels.iter().map(|x| x.id).collect();
            let before = ids.len();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(before, ids.len(), "{} has a duplicate panel id", p.id);
        }
    }

    #[test]
    fn every_panel_states_what_it_is_for() {
        // A panel with no note is a plot whose reader has to guess what it means.
        for p in all_presets() {
            for panel in &p.panels {
                assert!(!panel.note.trim().is_empty(), "{}/{}", p.id, panel.id);
                assert!(!panel.y_label.trim().is_empty(), "{}/{}", p.id, panel.id);
                if let Some(reason) = panel.blocked {
                    assert!(
                        reason.len() > 40,
                        "{}/{} is blocked with no explanation",
                        p.id,
                        panel.id
                    );
                }
            }
        }
    }

    #[test]
    fn a_blocked_panel_renders_a_placeholder_and_never_a_plot() {
        let spec = rq3();
        // Feed it rows for the blocked metric. A blocked panel must still refuse to plot,
        // because a metric name that collided with a blocked one would otherwise produce a
        // figure labelled as something it is not.
        let rows = vec![row(
            0,
            &[("actors.vehicles.demand.rate_veh_per_h", json!(1500.0))],
            "t95_enforce",
            "{}",
            Some(12.0),
        )];
        let set = render(&spec, &table(rows));
        let panel = set
            .figures
            .iter()
            .find(|f| f.id == "t95-enforce")
            .expect("the panel exists");
        assert!(!panel.is_plotted());
        assert_eq!(panel.points, 0);
        assert!(
            panel
                .unavailable
                .as_deref()
                .is_some_and(|u| u.starts_with(BLOCKED_PREFIX))
        );
        assert_eq!(set.blocked().len(), 3, "rq3 declares three blocked panels");
    }

    #[test]
    fn an_empty_panel_is_distinguishable_from_a_blocked_one() {
        // Nothing at all in the table: every unblocked panel must say "this run produced
        // no rows", which is a different diagnosis from "this build cannot measure it".
        let set = render(&rq1(), &table(Vec::new()));
        assert_eq!(set.plotted(), 0);
        assert_eq!(set.blocked().len(), 1);
        assert_eq!(set.empty().len(), set.figures.len() - 1);
    }

    #[test]
    fn a_distance_panel_plots_its_bins_in_numeric_order() {
        // Deliberately out of order in the table, and with a non-numeric bucket, so the
        // sort is doing work: a plain string sort would put "100-125" before "25-50" and
        // "unbinned" between them.
        let rows = vec![
            row(0, &[], "pdr", r#"{"dist_bin":"100-125"}"#, Some(0.4)),
            row(0, &[], "pdr", r#"{"dist_bin":"unbinned"}"#, Some(0.1)),
            row(0, &[], "pdr", r#"{"dist_bin":"25-50"}"#, Some(0.9)),
            row(0, &[], "pdr", r#"{"dist_bin":"50-75"}"#, Some(0.7)),
        ];
        let set = render(&rq1(), &table(rows));
        let panel = set
            .figures
            .iter()
            .find(|f| f.id == "pdr-vs-distance")
            .expect("the panel exists");
        assert!(panel.is_plotted());
        assert_eq!(panel.points, 4);
        assert_eq!(panel.traces, 1);
        let x = panel.plotly["data"][0]["x"]
            .as_array()
            .expect("an x array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert_eq!(x, vec!["25-50", "50-75", "100-125", "unbinned"]);
        // …and the y values follow their labels rather than staying in table order.
        let y = panel.plotly["data"][0]["y"]
            .as_array()
            .expect("a y array")
            .iter()
            .map(|v| v.as_f64().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(y, vec![0.9, 0.7, 0.4, 0.1]);
    }

    #[test]
    fn a_swept_parameter_becomes_one_trace_per_value() {
        let rows = vec![
            row(
                0,
                &[("actors.vehicles.demand.rate_veh_per_h", json!(1500.0))],
                "pdr",
                r#"{"dist_bin":"25-50"}"#,
                Some(0.95),
            ),
            row(
                1,
                &[("actors.vehicles.demand.rate_veh_per_h", json!(60000.0))],
                "pdr",
                r#"{"dist_bin":"25-50"}"#,
                Some(0.55),
            ),
        ];
        let set = render(&rq1(), &table(rows));
        let panel = set
            .figures
            .iter()
            .find(|f| f.id == "pdr-vs-distance")
            .expect("the panel exists");
        assert_eq!(panel.traces, 2);
        // Trace order is the label order, not the row order: "1500" before "60000".
        assert_eq!(panel.plotly["data"][0]["name"], json!("1500.0"));
        assert_eq!(panel.plotly["data"][1]["name"], json!("60000.0"));
    }

    #[test]
    fn a_row_with_no_estimate_is_not_plotted_as_a_zero() {
        // An insufficient bin has `mean: None`. Plotting it as 0.0 would put a fabricated
        // measurement on the figure, which is the failure this assertion exists to catch.
        let rows = vec![
            row(0, &[], "pdr", r#"{"dist_bin":"25-50"}"#, Some(0.9)),
            row(0, &[], "pdr", r#"{"dist_bin":"50-75"}"#, None),
        ];
        let set = render(&rq1(), &table(rows));
        let panel = set
            .figures
            .iter()
            .find(|f| f.id == "pdr-vs-distance")
            .expect("the panel exists");
        assert_eq!(panel.points, 1);
        let y = panel.plotly["data"][0]["y"].as_array().expect("a y array");
        assert_eq!(y.len(), 1);
        assert_eq!(y[0].as_f64(), Some(0.9));
    }

    #[test]
    fn the_error_bars_are_asymmetric_and_non_negative() {
        let rows = vec![row(0, &[], "pdr", r#"{"dist_bin":"25-50"}"#, Some(0.9))];
        let set = render(&rq1(), &table(rows));
        let panel = &set.figures[0];
        let error = &panel.plotly["data"][0]["error_y"];
        assert_eq!(error["symmetric"], json!(false));
        assert_eq!(error["array"][0].as_f64(), Some(0.02));
        assert_eq!(error["arrayminus"][0].as_f64(), Some(0.01));
    }

    #[test]
    fn every_float_on_a_figure_sits_on_the_declared_grid() {
        // Build decision D9, on the numbers that actually reach the file. 1/3 is the
        // adversarial case: unquantised it is 0.3333333333333333 and would fail.
        let rows = vec![row(
            0,
            &[],
            "pdr",
            r#"{"dist_bin":"25-50"}"#,
            Some(1.0 / 3.0),
        )];
        let set = render(&rq1(), &table(rows));
        let panel = &set.figures[0];
        for key in ["y"] {
            for v in panel.plotly["data"][0][key].as_array().expect("an array") {
                let x = v.as_f64().expect("a number");
                assert!(
                    v2xw_core::math::is_on_grid(x, Q_FIGURE),
                    "{x} is off the {Q_FIGURE} grid"
                );
            }
        }
        assert_eq!(
            panel.plotly["data"][0]["y"][0].as_f64(),
            Some(0.333333),
            "the estimate must be the quantised one, not the raw one"
        );
    }

    #[test]
    fn every_figure_carries_the_plan_digest() {
        // 08-measurement-and-data.md §1: "every figure carries it in metadata". Without
        // this a plot in a paper cannot be traced to the runs behind it.
        let rows = vec![row(0, &[], "pdr", r#"{"dist_bin":"25-50"}"#, Some(0.9))];
        let set = render(&rq1(), &table(rows));
        assert_eq!(set.plan_digest, "deadbeef");
        for figure in &set.figures {
            if figure.is_plotted() {
                assert_eq!(figure.plotly["v2xw"]["plan_digest"], json!("deadbeef"));
            }
        }
    }

    #[test]
    fn rendering_is_a_function_of_the_table() {
        let rows = vec![
            row(0, &[], "pdr", r#"{"dist_bin":"25-50"}"#, Some(0.9)),
            row(0, &[], "pdr", r#"{"dist_bin":"50-75"}"#, Some(0.7)),
        ];
        let once = render(&rq1(), &table(rows.clone()));
        let twice = render(&rq1(), &table(rows));
        assert_eq!(once, twice);
    }

    #[test]
    fn a_leading_number_is_found_where_there_is_one() {
        assert_eq!(leading_number("25-50"), Some(25.0));
        assert_eq!(leading_number("1500.0"), Some(1500.0));
        assert_eq!(leading_number("0.02"), Some(0.02));
        assert_eq!(leading_number("unbinned"), None);
        assert_eq!(leading_number("medium"), None);
        assert_eq!(leading_number(""), None);
        assert_eq!(leading_number("-"), None);
    }
}
