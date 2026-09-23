//! Scenario validation: 03-interfaces.md §13's "actionable errors".
//!
//! The requirement is specific, and it is the reason this module is not a pile of
//! `assert!`s: an error must name **the field** and **the conflict**, in the author's own
//! vocabulary, as in §13's own example —
//!
//! ```text
//! radio.tiers.phy: 'high' requires mac 'high' (mac is 'medium')
//! ```
//!
//! Every rule below produces that shape: [`ScenarioError::Conflict`] with a dotted
//! `field` a UI can highlight and a `conflict` that states the offending value, the rule
//! and the *other* value that the rule conflicts with. An error that says only "invalid
//! tier combination" sends the author back to the specification; this one does not.
//!
//! # All of them, not the first
//!
//! [`validate`] returns **every** problem, in a fixed order (the order of the schema's own
//! fields), because an author fixing a scenario wants the list rather than one round trip
//! per mistake. [`crate::scenario::Scenario::validate`] returns the first for callers that
//! only need pass or fail.

use std::collections::BTreeSet;

use serde_json::Value;
use v2xw_core::card::Tier;
use v2xw_core::time::WallClock;
use v2xw_core::weather::WeatherKind;

use crate::error::ScenarioError;
use crate::scenario::schema::{Scenario, TimelineKind};

/// The DES resolutions a scenario may promise its models.
const RESOLUTIONS: [&str; 3] = ["1ns", "1us", "1ms"];
/// The network layers `v2xw-net` implements.
const NET_LAYERS: [&str; 2] = ["wsmp", "gn-btp"];
/// The envelope profiles `v2xw-sec` implements.
const ENVELOPES: [&str; 2] = ["ieee1609.2", "etsi103097"];
/// The verification policies `v2xw-node` registers.
const POLICIES: [&str; 3] = ["verify-all", "on-demand", "prioritized"];
/// The codec tiers build decision D2 settled.
const CODEC_TIERS: [&str; 2] = ["uper", "size-model"];
/// The message sets `v2xw-msg` can generate or size.
const MESSAGE_SETS: [&str; 8] = ["bsm", "cam", "denm", "spat", "map", "psm", "vam", "cpm"];
/// The signature primitives that exist in `real` crypto mode.
///
/// `modeled` mode costs any primitive the profile prices; `real` mode has to do the
/// mathematics, and P-256 is the one `v2xw-sec` implements (the post-quantum primitives of
/// 05-protocols.md are sized, not computed).
const REAL_SIGNATURES: [&str; 1] = ["ecdsa-p256"];
/// The pseudonym-change strategies 05-protocols.md §2.6 names.
const STRATEGIES: [&str; 4] = ["time", "distance", "mix-zone", "silent"];

/// The on-board-unit hardware profiles that ship with `v2xw-node`.
///
/// Built from [`v2xw_node::profiles::PROFILE_SOURCES`] rather than listed, so a profile
/// added to that crate becomes selectable without an edit here. The filter is the id
/// prefix, which is how 06-node-models.md §7 spells a device's kind.
pub fn obu_profile_ids() -> Vec<&'static str> {
    v2xw_node::profiles::PROFILE_SOURCES
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| id.starts_with("obu/"))
        .collect()
}

/// Every hardware profile id that ships, for the roadside and backend slots.
pub fn all_profile_ids() -> Vec<&'static str> {
    v2xw_node::profiles::PROFILE_SOURCES
        .iter()
        .map(|(id, _)| *id)
        .collect()
}

// ---------------------------------------------------------------------------
// The machine-readable half of this module (13-product-direction.md §2)
// ---------------------------------------------------------------------------
//
// The page's settings form is generated, and its inline validation has to be the same
// validation the loader performs or the two disagree about what is valid. The three
// tables below are how: every numeric range and every closed value set is stated once,
// the rules further down read them, and `crate::scenario::publish` publishes them. A
// number that appears in a form and a different number in the loader is not possible,
// because there is only one number.

/// One numeric range the loader enforces and the published schema states.
#[derive(Debug, Clone, Copy)]
pub struct Bound {
    /// The dotted scenario path, with `[]` for a list element and `*` for a map key.
    pub path: &'static str,
    /// The lower limit.
    pub lo: f64,
    /// The upper limit; [`f64::INFINITY`] for "no upper limit", which publishes none.
    pub hi: f64,
    /// Whether `lo` itself is excluded.
    pub exclusive_lo: bool,
    /// What the quantity is, for the error message: "the equipped fraction is 1.5, …".
    pub what: &'static str,
}

impl Bound {
    /// The interval in mathematical notation, for an error message.
    pub fn describe(&self) -> String {
        let open = if self.exclusive_lo { '(' } else { '[' };
        if self.hi.is_infinite() {
            format!("{open}{}, ∞)", self.lo)
        } else {
            format!("{open}{}, {}]", self.lo, self.hi)
        }
    }

    /// Whether `value` satisfies it.
    pub fn admits(&self, value: f64) -> bool {
        let low = if self.exclusive_lo {
            value > self.lo
        } else {
            value >= self.lo
        };
        value.is_finite() && low && value <= self.hi
    }
}

/// Every numeric range in the schema.
///
/// A number that belongs in a range belongs here and nowhere else. `world.imported_at`,
/// `time.t0` and the cross-field rules are not ranges and stay as rules below.
pub static BOUNDS: &[Bound] = &[
    Bound { path: "time.duration_s", lo: 0.0, hi: f64::INFINITY, exclusive_lo: true,
            what: "the run length" },
    // ADR 0004 decision 2: below 10 ms no mobility provider is calibrated for the step,
    // and above 100 ms the constant-velocity extrapolation between steps stops being
    // accurate enough for frame-level radio.
    Bound { path: "time.mobility_step_ms", lo: 10.0, hi: 100.0, exclusive_lo: false,
            what: "the mobility period" },
    Bound { path: "world.buildings.metres_per_level", lo: 1.5, hi: 10.0,
            exclusive_lo: false, what: "the storey height" },
    Bound { path: "actors.vehicles.equipped_fraction", lo: 0.0, hi: 1.0,
            exclusive_lo: false, what: "the equipped fraction" },
    Bound { path: "actors.vehicles.classes.*.fraction", lo: 0.0, hi: 1.0,
            exclusive_lo: false, what: "the class share" },
    Bound { path: "actors.vehicles.demand.rate_veh_per_h", lo: 0.0, hi: f64::INFINITY,
            exclusive_lo: false, what: "the arrival rate" },
    Bound { path: "actors.vru.device_fraction", lo: 0.0, hi: 1.0, exclusive_lo: false,
            what: "the device fraction" },
    Bound { path: "actors.backend.links[].latency_ms", lo: 0.0, hi: f64::INFINITY,
            exclusive_lo: false, what: "the one-way latency" },
    Bound { path: "actors.backend.links[].capacity_mbps", lo: 0.0, hi: f64::INFINITY,
            exclusive_lo: true, what: "the link capacity" },
    // The same `[0, 1]` scale `v2xw_core::WeatherState::intensity` uses; each model's
    // card declares what its own 1.0 means.
    Bound { path: "weather.intensity", lo: 0.0, hi: 1.0, exclusive_lo: false,
            what: "the weather intensity" },
    Bound { path: "weather.visibility_m", lo: 0.0, hi: f64::INFINITY, exclusive_lo: true,
            what: "the meteorological visibility" },
    Bound { path: "radio.tiers.focus.region.radius_m", lo: 0.0, hi: f64::INFINITY,
            exclusive_lo: true, what: "the follow radius" },
    Bound { path: "security.pseudonym_change.period_s", lo: 1.0, hi: 86_400.0,
            exclusive_lo: false, what: "the rotation period" },
    // A rotation distance below a metre is not a distance and above a hundred kilometres
    // is longer than any trip this simulator places, so either is an author error rather
    // than a study.
    Bound { path: "security.pseudonym_change.distance_m", lo: 1.0, hi: 100_000.0,
            exclusive_lo: false, what: "the rotation distance" },
    Bound { path: "threats.attackers[].fraction", lo: 0.0, hi: 1.0, exclusive_lo: false,
            what: "the attacker fraction" },
];

/// One closed set of values the loader accepts and the published schema offers.
#[derive(Debug, Clone, Copy)]
pub struct Choices {
    /// The dotted scenario path.
    pub path: &'static str,
    /// The accepted values, in a stable order.
    pub values: &'static [&'static str],
    /// A path whose value narrows this set further, and the narrowing value; empty when
    /// the set is unconditional.
    ///
    /// `security.signature` is the case: `modeled` cryptography costs any primitive the
    /// hardware profile prices, and `real` has to do the mathematics. A form can offer
    /// the wide set and grey out what the current mode cannot do, which is what the
    /// loader enforces.
    pub narrowed_by: &'static str,
    /// The value of `narrowed_by` that applies the narrowing.
    pub narrowed_when: &'static str,
    /// The narrower set, when `narrowed_by` is set.
    pub narrowed_to: &'static [&'static str],
}

/// A choice set with no conditional narrowing.
const fn choices(path: &'static str, values: &'static [&'static str]) -> Choices {
    Choices { path, values, narrowed_by: "", narrowed_when: "", narrowed_to: &[] }
}

/// Every closed value set in the schema whose members are fixed strings.
///
/// Sets whose members come from a registry — model ids, hardware profile ids — are
/// published as slots by `crate::scenario::publish` instead, because their membership is
/// a property of the build rather than a constant.
pub static CHOICES: &[Choices] = &[
    choices("time.des_resolution", &RESOLUTIONS),
    choices("net.layer", &NET_LAYERS),
    choices("messages.codec_tier", &CODEC_TIERS),
    choices("messages.sets[]", &MESSAGE_SETS),
    choices("security.envelope", &ENVELOPES),
    choices("security.verification_policy", &POLICIES),
    choices("security.pseudonym_change.strategy", &STRATEGIES),
    Choices {
        path: "security.signature",
        values: &["ecdsa-p256"],
        narrowed_by: "security.crypto_mode",
        narrowed_when: "real",
        narrowed_to: &REAL_SIGNATURES,
    },
];

/// How much of a scenario key this build actually acts on.
///
/// The distinction the page needs is not "valid or invalid" — the loader answers that —
/// but "will editing this change the run". 13-product-direction.md §2 is explicit that a
/// field the engine does not act on must not be offered as though it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// The engine reads it and it changes the run.
    Wired,
    /// The engine reads it and acts on part of it, or acts on it only under conditions
    /// the note states. Editing it may do less than it appears to.
    Partial,
    /// Validated, hashed, and read by nothing. Editing it changes the scenario hash and
    /// nothing else.
    NotImplemented,
    /// Validated and refused: the loader rejects any value this build cannot act on, so
    /// the field exists but only its implemented values load.
    Refused,
}

/// One key's implementation status.
#[derive(Debug, Clone, Copy)]
pub struct KeyStatus {
    /// The dotted path, or a prefix of one. The longest matching entry wins, so a
    /// section can be classified once and an exception stated beneath it.
    pub path: &'static str,
    /// How much of it is real.
    pub status: Status,
    /// One line a non-specialist can read, saying what happens if they edit it.
    pub note: &'static str,
}

/// The implementation status of every scenario key.
///
/// Written from the 2026-09-22 wiring audit, which traced every leaf of the schema to
/// the code that reads it. Entries are prefixes, longest match wins, and
/// `crate::scenario::publish`'s `every_leaf_has_a_status` test fails the build if a leaf
/// of the reflected schema matches none of them — so a field added to the schema cannot
/// reach the page unclassified.
pub static KEY_STATUS: &[KeyStatus] = &[
    // --- the run -----------------------------------------------------------
    KeyStatus { path: "schema", status: Status::Wired,
        note: "The schema version. The loader migrates by it." },
    KeyStatus { path: "meta", status: Status::NotImplemented,
        note: "Documentation. It is hashed into the scenario digest and changes nothing \
               about the run." },
    KeyStatus { path: "meta.name", status: Status::Wired,
        note: "Names the output directory, the recording's run label and the manifest." },
    KeyStatus { path: "meta.base", status: Status::Wired,
        note: "The scenario this one overlays; resolved by the loader before anything \
               else." },
    KeyStatus { path: "seed", status: Status::Wired,
        note: "The master seed. Every random draw in the run derives from it." },
    KeyStatus { path: "time.t0", status: Status::Wired,
        note: "The civil instant simulated time zero is; every 1609.2 generationTime is \
               stamped from it." },
    KeyStatus { path: "time.duration_s", status: Status::Wired,
        note: "The run horizon." },
    KeyStatus { path: "time.mobility_step_ms", status: Status::Wired,
        note: "The mobility period, and the step the stream and run.seek are quantised \
               to." },
    KeyStatus { path: "time.des_resolution", status: Status::NotImplemented,
        note: "The kernel is always nanoseconds. This states a guarantee to models and \
               no model reads it." },
    KeyStatus { path: "time.time_dilation", status: Status::NotImplemented,
        note: "Recorded in the manifest and nowhere else: no radio event is skipped \
               inside a window and no metric is marked not-observed for one." },
    // --- the world ---------------------------------------------------------
    KeyStatus { path: "world.source", status: Status::Wired,
        note: "Where the world comes from. Procedural grids and OpenStreetMap XML are \
               built; the other source kinds return an unsupported-source error." },
    KeyStatus { path: "world.imported_at", status: Status::Wired,
        note: "The import date the world's provenance records. Supplied here because no \
               part of the engine may read a clock." },
    KeyStatus { path: "world.buildings.enabled", status: Status::Wired,
        note: "Whether buildings obstruct radio links. On, a link through a building \
               loses 9 dB per wall and 0.4 dB per metre inside (Sommer 2011), capped at \
               the around-the-corner street-canyon loss of 3GPP TR 37.885's urban NLOS \
               law; off, every link is line of sight. Buildings are imported and drawn \
               either way. Applies at the medium and high propagation tiers." },
    KeyStatus { path: "world.buildings.keep_holes", status: Status::Wired,
        note: "Whether interior courtyards stay holes in a footprint." },
    KeyStatus { path: "world.buildings.metres_per_level", status: Status::Wired,
        note: "Overrides the importer's storey height for buildings tagged with levels \
               rather than a height." },
    KeyStatus { path: "world.terrain", status: Status::Wired,
        note: "A digital elevation model (an SRTM .hgt tile or a geographic ESRI ASCII \
               grid) is read, resampled onto the world, and obstructs radio links by \
               ITU-R P.526 knife-edge diffraction over the ground profile. Roads and \
               buildings are not lifted onto it. No file: the world is flat." },
    KeyStatus { path: "world.cache", status: Status::NotImplemented,
        note: "No import is cached. Every run re-imports the world." },
    KeyStatus { path: "world.highway_preset", status: Status::Wired,
        note: "Which jurisdiction's fallback speed limits the OpenStreetMap importer \
               uses. An OSM import is refused without it." },
    // --- what moves --------------------------------------------------------
    KeyStatus { path: "actors.vehicles.demand.kind", status: Status::Partial,
        note: "Only 'mobility/demand/none' is distinguished. Every other id runs the \
               thinned-Poisson model, so naming another demand model selects Poisson." },
    KeyStatus { path: "actors.vehicles.demand.rate_veh_per_h", status: Status::Wired,
        note: "Vehicles per hour offered to the network." },
    KeyStatus { path: "actors.vehicles.demand.params", status: Status::Partial,
        note: "Deserialised as the thinned-Poisson model's parameters and nothing else. \
               'max_total_vehicles' is the only way to ask for an exact fleet size." },
    KeyStatus { path: "actors.vehicles.equipped_fraction", status: Status::Wired,
        note: "What share of vehicles carry a radio. 0 is a legal pure-traffic run." },
    KeyStatus { path: "actors.vehicles.classes", status: Status::NotImplemented,
        note: "The shares are validated to sum to 1 and then ignored: the fleet mix comes \
               from the demand model's own 'fleet' parameter, which defaults to cars \
               only." },
    KeyStatus { path: "actors.vru", status: Status::Refused,
        note: "Nothing in this build spawns a pedestrian or a cyclist, so the loader \
               refuses any value above zero rather than reporting no VRU traffic without \
               saying why." },
    KeyStatus { path: "actors.rsus", status: Status::Wired,
        note: "Roadside units. Placed, given a profile and a role set, and they transmit." },
    KeyStatus { path: "actors.rsus[].backhaul", status: Status::NotImplemented,
        note: "The backhaul latency in force is a fixed constant from the SCMS \
               parameters; this id is read by nothing." },
    KeyStatus { path: "actors.backend.protocol", status: Status::Wired,
        note: "The credential-management protocol. Naming the CAMP SCMS is what turns the \
               whole backend on." },
    KeyStatus { path: "actors.backend.entities", status: Status::NotImplemented,
        note: "The backend runs on fixed built-in parameters. Per-entity profiles, \
               service models and network models are read by nothing." },
    KeyStatus { path: "actors.backend.links", status: Status::NotImplemented,
        note: "Validated as a topology and then ignored: every backend hop uses one \
               constant latency and no capacity limit." },
    // --- environment -------------------------------------------------------
    KeyStatus { path: "weather.initial", status: Status::Partial,
        note: "Reaches the propagation model and the GNSS error model. It changes no \
               driving behaviour, because the mobility provider is never told the \
               weather." },
    KeyStatus { path: "weather.intensity", status: Status::Partial,
        note: "Only the high-tier propagation model reads it, and only for rain and \
               sleet. At the default medium tier it changes nothing." },
    KeyStatus { path: "weather.visibility_m", status: Status::NotImplemented,
        note: "Carried into the weather state and read by no model that runs." },
    KeyStatus { path: "weather.surface", status: Status::NotImplemented,
        note: "No model that runs reads the road surface condition." },
    // --- radio -------------------------------------------------------------
    KeyStatus { path: "radio.rat", status: Status::Wired,
        note: "The radio access technology. dsrc-80211p runs CSMA/CA with J2945/1 \
               congestion control on channel 172; lte-v2x-pc5 runs Mode 4 sensing-based \
               semi-persistent scheduling on a 10 MHz, four-sub-channel pool in channel \
               183; nr-v2x-pc5 runs Mode 2 at 30 kHz with re-evaluation and pre-emption. \
               'hybrid' is refused: it needs a per-message policy no key states." },
    KeyStatus { path: "radio.tiers.propagation", status: Status::Wired,
        note: "Path-loss fidelity. Abstract is free-space; medium and high are \
               log-distance with shadowing." },
    KeyStatus { path: "radio.tiers.phy", status: Status::Wired,
        note: "Physical-layer fidelity." },
    KeyStatus { path: "radio.tiers.mac", status: Status::Wired,
        note: "Medium-access fidelity." },
    KeyStatus { path: "radio.tiers.focus", status: Status::Partial,
        note: "A region — a disc following one node, or a map box — whose links run the \
               propagation and the receiver at the focus tier (at high: weather \
               attenuation and preamble capture). Links entering it use the surrounding \
               propagation with no fading draw. Medium access stays one model for the \
               whole world, and 802.11p only: a sidelink run ignores the region's PHY tier." },
    KeyStatus { path: "radio.models", status: Status::Wired,
        note: "Picks a model per radio family, overriding the tier's default: \
               propagation (free-space, two-ray-ground, log-distance with a named preset, \
               tr37885), fading (none, nakagami-m with a preset), per (the 802.11p error \
               model's implementation loss), phy (the 802.11p sensitivity table) and \
               obstacle (the Sommer building row). Unknown families and ids are refused." },
    // --- network -----------------------------------------------------------
    KeyStatus { path: "net.layer", status: Status::Refused,
        note: "No network layer is composed: a frame goes from the signer to the MAC with \
               no header between them. Only 'wsmp' loads, and even that is a declaration." },
    KeyStatus { path: "net.fragmenter", status: Status::NotImplemented,
        note: "No fragmenter is wired in; a frame over the MSDU cap is dropped rather \
               than split." },
    KeyStatus { path: "net.backhaul", status: Status::NotImplemented,
        note: "Read by nothing." },
    KeyStatus { path: "net.uu", status: Status::NotImplemented,
        note: "Read by nothing: there is no cellular uplink in this build." },
    KeyStatus { path: "net.backend_net", status: Status::NotImplemented,
        note: "Read by nothing." },
    // --- messages ----------------------------------------------------------
    KeyStatus { path: "messages.sets", status: Status::Refused,
        note: "Which message sets the nodes generate. Only the BSM and the CAM have a \
               generator, so anything else is refused rather than silently unsent." },
    KeyStatus { path: "messages.generator", status: Status::Partial,
        note: "Sets where each node's generator sits in time: params.phase_window_ms (each \
               node's phase is uniform over it; default 100) and params.max_jitter_ms (a \
               per-message delay before the radio; default 10). 0 and 0 put every vehicle \
               on one grid. The generation rules themselves are fixed." },
    KeyStatus { path: "messages.codec_tier", status: Status::Refused,
        note: "The node encodes real UPER unconditionally, so the size-model tier is \
               refused rather than ignored." },
    // --- security ----------------------------------------------------------
    KeyStatus { path: "security.envelope", status: Status::Wired,
        note: "Which secured-message envelope the nodes use." },
    KeyStatus { path: "security.protocol", status: Status::NotImplemented,
        note: "The credential protocol is selected by actors.backend.protocol. This key \
               is read by nothing." },
    KeyStatus { path: "security.signature", status: Status::NotImplemented,
        note: "Cross-checked against the crypto mode and then ignored: the primitive is \
               fixed in the security crate." },
    KeyStatus { path: "security.crypto_mode", status: Status::Wired,
        note: "Whether signing and verification are costed or actually computed. Both \
               produce the same event log; only the manifest and the timing differ." },
    KeyStatus { path: "security.verification_policy", status: Status::Partial,
        note: "The policy is selected, but its threshold is a fixed number: there is no \
               scenario key for the on-demand relevance threshold or the prioritised \
               range." },
    KeyStatus { path: "security.signer_id_policy", status: Status::Wired,
        note: "How often a full certificate is attached instead of an eight-byte digest." },
    KeyStatus { path: "security.pseudonym_change.strategy", status: Status::NotImplemented,
        note: "The engine's rotation rule is built from the period and the distance below; \
               the strategy name is validated and then not read, so 'silent' still \
               rotates." },
    KeyStatus { path: "security.pseudonym_change.period_s", status: Status::Partial,
        note: "Sets the minimum age before a node may change pseudonym. A change also \
               needs a credential to change to, and without a backend protocol each node \
               holds exactly one." },
    KeyStatus { path: "security.pseudonym_change.distance_m", status: Status::Partial,
        note: "Sets the minimum distance before a node may change pseudonym, with the \
               same caveat about the credential pool." },
    // --- nodes -------------------------------------------------------------
    KeyStatus { path: "nodes.default_obu", status: Status::Wired,
        note: "The hardware profile every equipped vehicle runs on: its compute, its \
               security module and its radio." },
    KeyStatus { path: "nodes.per_class", status: Status::Wired,
        note: "Per-vehicle-class overrides of the profile above." },
    KeyStatus { path: "nodes.compute_tier", status: Status::Partial,
        note: "abstract: signing and verification cost a microsecond and no node is ever \
               compute-bound. medium and high: every operation costs the hardware \
               profile's service time and queues behind the node's servers; high adds \
               nothing over medium yet." },
    KeyStatus { path: "nodes.backend_tier", status: Status::NotImplemented,
        note: "Read by nothing." },
    // --- threats and detection --------------------------------------------
    KeyStatus { path: "threats.attackers", status: Status::Wired,
        note: "Attacker populations: which model, how many, and when they are active." },
    KeyStatus { path: "threats.attackers[].params", status: Status::Partial,
        note: "Only 'intensity' and 'dt_s' are read; any other key in the object is \
               silently dropped." },
    KeyStatus { path: "threats.jammers", status: Status::Wired,
        note: "Fixed-position jammers: constant, pulsed (period_ms, duty) or reactive \
               (trigger_dbm), with position_m, power_dbm and an active window. Their \
               energy raises the noise at every receiver in range, holds 802.11p carrier \
               sense busy, counts as channel load, and a frame they kill is reported \
               'jammed'." },
    KeyStatus { path: "threats.compromised_rsus", status: Status::NotImplemented,
        note: "Read by nothing." },
    KeyStatus { path: "detection.local", status: Status::Partial,
        note: "The detector suite is installed by id. Its parameters are not read." },
    KeyStatus { path: "detection.ma", status: Status::NotImplemented,
        note: "The misbehaviour authority runs on built-in parameters; naming a pipeline \
               model selects nothing." },
    KeyStatus { path: "detection.responder", status: Status::NotImplemented,
        note: "Read by nothing." },
    KeyStatus { path: "detection.perception_tier", status: Status::NotImplemented,
        note: "Read by nothing: there is no perception model in this build." },
    // --- measurement -------------------------------------------------------
    KeyStatus { path: "metrics", status: Status::Wired,
        note: "Which metric providers to install; 'all' selects every registered one." },
    KeyStatus { path: "exporters", status: Status::Refused,
        note: "The engine runs no exporter stage. Exporting is the command line's job, so \
               a non-empty list is refused rather than ignored." },
    KeyStatus { path: "events", status: Status::Partial,
        note: "Timeline items are scheduled and fire. Only 'outage' and 'weather.front' \
               do anything; a demand multiplier, an attack wave, a parameter change and a \
               closure are counted and change nothing." },
    KeyStatus { path: "experiment", status: Status::Wired,
        note: "The parameter sweep. Expanded by the experiment runner, not by a single \
               run: the engine clears it before running a cell." },
];

/// The status entry that governs `path`: the longest matching prefix.
pub fn status_of(path: &str) -> Option<&'static KeyStatus> {
    KEY_STATUS
        .iter()
        .filter(|k| covers(k.path, path))
        .max_by_key(|k| k.path.len())
}

/// Whether the status entry `entry` governs the leaf `path`.
///
/// A prefix match is on a path segment boundary, so `net.layer` does not govern
/// `net.layerx` and `meta` governs `meta.tags`.
fn covers(entry: &str, path: &str) -> bool {
    if path == entry {
        return true;
    }
    let Some(rest) = path.strip_prefix(entry) else {
        return false;
    };
    rest.starts_with('.') || rest.starts_with('[')
}

/// The bound declared for `path`, if there is one.
pub fn bound_of(path: &str) -> Option<&'static Bound> {
    BOUNDS.iter().find(|b| b.path == path)
}

/// The choice set declared for `path`, if there is one.
pub fn choices_of(path: &str) -> Option<&'static Choices> {
    CHOICES.iter().find(|c| c.path == path)
}


/// Every problem with `s`, in schema order. Empty means the scenario is loadable.
pub fn validate(s: &Scenario) -> Vec<ScenarioError> {
    let mut e = Vec::new();
    time(s, &mut e);
    world(s, &mut e);
    actors(s, &mut e);
    weather(s, &mut e);
    radio(s, &mut e);
    net(s, &mut e);
    messages(s, &mut e);
    security(s, &mut e);
    nodes(s, &mut e);
    threats(s, &mut e);
    metrics_and_exporters(s, &mut e);
    timeline(s, &mut e);
    experiment(s, &mut e);
    unreachable_keys(s, &mut e);
    e
}

/// Keys this build validates, documents and hashes but **cannot act on**.
///
/// The vertical-slice audit's finding, in one function: a scenario that says something the
/// engine never reads is a scenario that overstates what it controls, and a run that
/// accepted it produced a result the file appears to explain and does not. Everything
/// here is refused rather than warned about, because a warning in a log is a warning
/// nobody reads and the whole point of §13's validation is that the author finds out at
/// load rather than at analysis.
///
/// Each message names the *seam* — the model or the field that would have to exist — so
/// that removing a rule from here is a one-line change the day the seam is filled, and so
/// that a reader can tell "not implemented" from "not allowed".
///
/// The six keys the audit found are wired rather than refused and are **not** here:
/// `messages.sets` reaches [`crate::wiring::service_set`], `security.crypto_mode`
/// reaches the node's crypto backend, `security.envelope` and
/// `security.signer_id_policy` reach its security stack, `nodes.per_class` reaches its
/// hardware profile, and `time.t0` reaches its wall clock through
/// [`crate::wiring::NodeEnv`].
fn unreachable_keys(s: &Scenario, e: &mut Vec<ScenarioError>) {
    // Vulnerable road users are a mobility population. `v2xw-mobility` spawns vehicles
    // from a demand model and nothing spawns a pedestrian or a cyclist, so the three
    // fields select a population that never exists.
    let vru = &s.actors.vru;
    if vru.pedestrians > 0 || vru.cyclists > 0 || vru.device_fraction > 0.0 {
        e.push(conflict(
            "actors.vru",
            format!(
                "asks for {} pedestrians and {} cyclists at a device fraction of {}, and                  nothing in this build spawns a vulnerable road user: the mobility provider                  creates vehicles from `actors.vehicles.demand` only, so these three fields                  change nothing and the run would report no VRU traffic without saying why",
                vru.pedestrians, vru.cyclists, vru.device_fraction
            ),
        ));
    }

    // Exporters run after a run, from the command-line tool's own options; the engine has
    // no exporter stage and `Scenario::exporters` reaches nothing.
    if !s.exporters.is_empty() {
        e.push(conflict(
            "exporters",
            format!(
                "lists {} exporter(s), and the engine runs none: exporting is the                  command-line tool's stage (`v2xw run --record`, `v2xw export`) and this                  list is not read by `Engine::run`. Drive the exporter from the tool                  instead of from the scenario",
                s.exporters.len()
            ),
        ));
    }

    // The network layer. `v2xw-net` implements both, and the engine composes neither:
    // `Engine::hand_down_app` puts a signed SPDU straight into the MAC's queue with no
    // WSMP or GeoNetworking header between them.
    if s.net.layer != "wsmp" {
        e.push(conflict(
            "net.layer",
            format!(
                "is '{}', and this build composes no network layer at all: a frame goes                  from the node's signer to the MAC with no header between them, so                  'gn-btp' would not be the thing that ran. Only 'wsmp' is accepted, and                  even that is a declaration rather than a layer — see `Engine::hand_down_app`",
                s.net.layer
            ),
        ));
    }

    // Message generators. `v2xw-node::ServiceSet` has exactly two flags, so a set outside
    // those two names a generator that does not exist. This is a different rule from the
    // codec check in `messages`: that one is about encoding a message, this one is about
    // deciding to send it.
    for (i, set) in s.messages.sets.iter().enumerate() {
        if !matches!(set.as_str(), "bsm" | "cam") {
            e.push(conflict(
                &format!("messages.sets[{i}]"),
                format!(
                    "'{set}' has no generator: `v2xw_node::ServiceSet` carries a flag for                      the CAM and a flag for the BSM and nothing else, so a node would never                      decide to send one. 06-node-models.md §2.1's application layer is the                      seam; until it ships, only 'bsm' and 'cam' reach the air"
                ),
            ));
        }
    }

    // The codec tier. `ObuRuntime` constructs an `EtsiUperCodec` and calls the J2735 BSM
    // encoder directly; `v2xw_msg::J2735SizeCodec` exists and nothing selects it.
    if s.messages.codec_tier != "uper" {
        e.push(conflict(
            "messages.codec_tier",
            format!(
                "is '{}', and the node runtime encodes real UPER unconditionally: it holds                  an `EtsiUperCodec` and calls the J2735 BSM encoder directly, and nothing                  reads this field to choose `v2xw_msg::J2735SizeCodec` instead. Build                  decision D2's size model is reachable through `v2xw-msg` and not through a                  scenario, so 'size-model' here would select nothing",
                s.messages.codec_tier
            ),
        ));
    }
}

fn conflict(field: &str, conflict: String) -> ScenarioError {
    ScenarioError::conflict(field, conflict)
}

/// `value` satisfies the bound [`BOUNDS`] declares for `path`, or an error saying so.
///
/// `path` is the *canonical* path — `threats.attackers[].fraction` — and `field` is the
/// concrete one the author wrote — `threats.attackers[2].fraction` — so the table has one
/// row per rule and the message names the author's own field.
fn bounded_at(path: &str, field: &str, value: f64, e: &mut Vec<ScenarioError>) {
    let Some(b) = bound_of(path) else {
        // A field checked through this helper with no row in the table is a defect in
        // this module: the page would offer an unbounded control for a bounded field.
        // It is reported rather than skipped, because a check that cannot fail is the
        // defect class this project has already found four times.
        e.push(conflict(
            field,
            format!(
                "cannot be range-checked: `{path}` has no row in `validate::BOUNDS`, which                  is a defect in the engine rather than in this scenario"
            ),
        ));
        return;
    };
    if !b.admits(value) {
        e.push(conflict(
            field,
            format!(
                "{} is {value}, which is outside the allowed range {}",
                b.what,
                b.describe()
            ),
        ));
    }
}

/// One of a fixed set, or an error listing the set.
fn one_of(field: &str, value: &str, allowed: &[&str], e: &mut Vec<ScenarioError>) {
    if !allowed.contains(&value) {
        e.push(conflict(
            field,
            format!(
                "'{value}' is not one this build implements; allowed: {}",
                allowed.join(", ")
            ),
        ));
    }
}

fn time(s: &Scenario, e: &mut Vec<ScenarioError>) {
    if WallClock::parse_rfc3339(&s.time.t0).is_err() {
        e.push(conflict(
            "time.t0",
            format!(
                "'{}' is not an RFC 3339 instant of the form 2027-03-04T07:00:00Z; it is what \
                 every 1609.2 generationTime is stamped from, so it cannot be guessed",
                s.time.t0
            ),
        ));
    }
    bounded_at("time.duration_s", "time.duration_s", s.time.duration_s, e);
    // ADR 0004 decision 2's window, read from `BOUNDS` so the page's slider and this
    // check cannot disagree about it.
    if let Some(b) = bound_of("time.mobility_step_ms")
        && !b.admits(s.time.mobility_step_ms as f64)
    {
        e.push(conflict(
            "time.mobility_step_ms",
            format!(
                "is {}, and ADR 0004 decision 2 allows {} ms: below the floor no mobility \
                 provider is calibrated for the step, and above the ceiling the published \
                 constant-velocity extrapolation between steps stops being accurate enough \
                 for frame-level radio",
                s.time.mobility_step_ms,
                b.describe()
            ),
        ));
    }
    one_of(
        "time.des_resolution",
        &s.time.des_resolution,
        &RESOLUTIONS,
        e,
    );

    let horizon = s.time.duration_s;
    let mut windows: Vec<(f64, f64)> = Vec::new();
    for (i, w) in s.time.time_dilation.iter().enumerate() {
        let field = format!("time.time_dilation[{i}]");
        if w.to_s <= w.from_s {
            e.push(conflict(
                &field,
                format!(
                    "to_s is {} but from_s is {}; a window ends after it starts",
                    w.to_s, w.from_s
                ),
            ));
            continue;
        }
        if w.from_s < 0.0 || w.to_s > horizon {
            e.push(conflict(
                &field,
                format!(
                    "spans [{}, {}] s but the run is [0, {}] s (time.duration_s); a window \
                     outside the run silently disables nothing",
                    w.from_s, w.to_s, horizon
                ),
            ));
        }
        for (j, prev) in windows.iter().enumerate() {
            if w.from_s < prev.1 && prev.0 < w.to_s {
                e.push(conflict(
                    &field,
                    format!(
                        "overlaps time.time_dilation[{j}] ([{}, {}] s): the manifest records \
                         windows as disjoint intervals and metrics are marked not-observed \
                         per window, so an overlap has no single answer",
                        prev.0, prev.1
                    ),
                ));
            }
        }
        windows.push((w.from_s, w.to_s));
    }
}

fn world(s: &Scenario, e: &mut Vec<ScenarioError>) {
    // 02-architecture.md §6.1: nothing in the engine reads a wall clock, so an importer
    // cannot date its own work. A generated world has no import to date.
    let needs_date = !matches!(
        s.world.source,
        v2xw_world::WorldSourceSpec::Procedural { .. }
    );
    if needs_date && s.world.imported_at.trim().is_empty() {
        e.push(conflict(
            "world.imported_at",
            format!(
                "is empty, but world.source is {} — an imported world records the import \
                 date in its provenance and no part of the engine may read a wall clock \
                 (02-architecture.md §6.1), so the scenario supplies it",
                s.world.source.label()
            ),
        ));
    }
    if let Some(mpl) = s.world.buildings.metres_per_level {
        bounded_at(
            "world.buildings.metres_per_level",
            "world.buildings.metres_per_level",
            mpl,
            e,
        );
    }
}

fn actors(s: &Scenario, e: &mut Vec<ScenarioError>) {
    bounded_at(
        "actors.vehicles.equipped_fraction",
        "actors.vehicles.equipped_fraction",
        s.actors.vehicles.equipped_fraction,
        e,
    );
    bounded_at(
        "actors.vru.device_fraction",
        "actors.vru.device_fraction",
        s.actors.vru.device_fraction,
        e,
    );

    if !s.actors.vehicles.classes.is_empty() {
        let mut total = 0.0;
        for (name, c) in &s.actors.vehicles.classes {
            bounded_at(
                "actors.vehicles.classes.*.fraction",
                &format!("actors.vehicles.classes.{name}.fraction"),
                c.fraction,
                e,
            );
            total += c.fraction;
        }
        // 1e-9 is the project's cross-engine float tolerance (D9); anything looser would
        // let a fleet quietly lose vehicles.
        if (total - 1.0).abs() > 1e-9 {
            e.push(conflict(
                "actors.vehicles.classes",
                format!(
                    "the class shares sum to {total}, not 1.0; every vehicle belongs to \
                     exactly one class, so a sum below 1 leaves vehicles with no class and \
                     a sum above 1 makes the mix unreachable"
                ),
            ));
        }
    }

    if let Some(rate) = s.actors.vehicles.demand.rate_veh_per_h
        && !(rate.is_finite() && rate >= 0.0)
    {
        e.push(conflict(
            "actors.vehicles.demand.rate_veh_per_h",
            format!("is {rate}, and an arrival rate is a finite non-negative number"),
        ));
    }

    let profiles = all_profile_ids();
    let mut seen_sites = BTreeSet::new();
    for (i, r) in s.actors.rsus.iter().enumerate() {
        if let Some(profile) = &r.profile
            && !profiles.contains(&profile.as_str())
        {
            e.push(conflict(
                &format!("actors.rsus[{i}].profile"),
                format!(
                    "'{profile}' is not a hardware profile this build ships, and an \
                     unknown one silently becomes the reference on-board unit; shipped \
                     profiles: {}",
                    profiles.join(", ")
                ),
            ));
        }
        match (r.site, r.position_m) {
            (Some(site), None) => {
                if !seen_sites.insert(site) {
                    e.push(conflict(
                        &format!("actors.rsus[{i}].site"),
                        format!(
                            "site {site} already carries a roadside unit; two units at one \
                             site would share a position and the node ids would be assigned \
                             by list order"
                        ),
                    ));
                }
            }
            (None, Some(p)) => {
                if p.iter().any(|v| !v.is_finite()) {
                    e.push(conflict(
                        &format!("actors.rsus[{i}].position_m"),
                        "is not a finite world-local position".to_string(),
                    ));
                }
            }
            (Some(_), Some(_)) => e.push(conflict(
                &format!("actors.rsus[{i}]"),
                "names both a world `site` and an explicit `position_m`; give exactly one, \
                 because two answers to where the mast stands is one answer too many"
                    .to_string(),
            )),
            (None, None) => e.push(conflict(
                &format!("actors.rsus[{i}]"),
                "says where it stands neither by world `site` nor by `position_m`; an OSM \
                 import carries no mast inventory, so a scenario on one states the position"
                    .to_string(),
            )),
        }
    }

    let entities: BTreeSet<&str> = s
        .actors
        .backend
        .entities
        .keys()
        .map(String::as_str)
        .collect();
    for (i, l) in s.actors.backend.links.iter().enumerate() {
        if let Some(latency) = l.latency_ms {
            bounded_at(
                "actors.backend.links[].latency_ms",
                &format!("actors.backend.links[{i}].latency_ms"),
                latency,
                e,
            );
        }
        if let Some(capacity) = l.capacity_mbps {
            bounded_at(
                "actors.backend.links[].capacity_mbps",
                &format!("actors.backend.links[{i}].capacity_mbps"),
                capacity,
                e,
            );
        }
        for (end, name) in [("from", &l.from), ("to", &l.to)] {
            if !entities.contains(name.as_str()) {
                e.push(conflict(
                    &format!("actors.backend.links[{i}].{end}"),
                    format!(
                        "'{name}' is not in actors.backend.entities (which has {})",
                        if entities.is_empty() {
                            "no entries".to_string()
                        } else {
                            entities.iter().copied().collect::<Vec<_>>().join(", ")
                        }
                    ),
                ));
            }
        }
    }
}

/// Weather at `t0`.
///
/// The two ranges here were the gap the schema's own header forbids: `intensity` and
/// `visibility_m` were defaulted and unchecked, so a scenario could state an intensity of
/// 40 and the propagation model would be handed it. A generated form would have offered
/// an unbounded number box for a `[0, 1]` quantity.
fn weather(s: &Scenario, e: &mut Vec<ScenarioError>) {
    bounded_at("weather.intensity", "weather.intensity", s.weather.intensity, e);
    if let Some(v) = s.weather.visibility_m {
        bounded_at("weather.visibility_m", "weather.visibility_m", v, e);
    }
}

fn radio(s: &Scenario, e: &mut Vec<ScenarioError>) {
    let t = &s.radio.tiers;

    // `radio.models`: every family and id must be one `wiring::build_radio`,
    // `build_phy` and `build_obstacles` act on, with parameters that fit the model. The
    // parse is the wiring's own, so the loader and the run cannot disagree about it.
    if let Err(problems) = crate::wiring::radio_models(s) {
        for (path, why) in problems {
            e.push(conflict(&path, why));
        }
    }

    // `hybrid` names two radios on one node and a policy choosing between them per
    // message. `v2xw_radio::hybrid` has the selector; nothing states the policy, so the
    // engine would have to invent one.
    if s.radio.rat == crate::scenario::schema::Rat::Hybrid {
        e.push(conflict(
            "radio.rat",
            "is 'hybrid', which needs a per-message arbitration policy between the \
             802.11p and the sidelink stack that no scenario key states; choose \
             dsrc-80211p, lte-v2x-pc5 or nr-v2x-pc5"
                .to_string(),
        ));
    }

    // 03-interfaces.md §13's own example, and 02-architecture.md §7.1's ladder: a
    // frame-level PHY decides receptions per frame, and a MAC below `high` does not
    // produce frames at that granularity, so the PHY would be computing outcomes for
    // arrivals the MAC never scheduled.
    if t.phy == Tier::High && t.mac != Tier::High {
        e.push(conflict(
            "radio.tiers.phy",
            format!(
                "'high' requires mac 'high' (mac is '{}'): a frame-level PHY decides an \
                 outcome per frame, and a '{}' MAC does not schedule frames at that \
                 granularity",
                t.mac, t.mac
            ),
        ));
    }
    if t.propagation == Tier::Abstract && t.phy != Tier::Abstract {
        e.push(conflict(
            "radio.tiers.propagation",
            format!(
                "'abstract' conflicts with phy '{}': an abstract propagation model returns a \
                 reception probability rather than a received power, and a '{}' PHY needs a \
                 power to accumulate SINR from",
                t.phy, t.phy
            ),
        ));
    }
    if let Some(f) = &t.focus {
        let base = t.phy.max(t.mac).max(t.propagation);
        if f.tier <= base {
            e.push(conflict(
                "radio.tiers.focus.tier",
                format!(
                    "is '{}' but the surrounding world already runs at '{base}' \
                     (radio.tiers): a focus region exists to run *higher* than its \
                     surroundings (02-architecture.md §7.3), so this one costs the mixed-tier \
                     boundary and buys nothing",
                    f.tier
                ),
            ));
        }
        if let crate::scenario::schema::FocusRegion::Follow { radius_m, .. } = f.region
        {
            bounded_at(
                "radio.tiers.focus.region.radius_m",
                "radio.tiers.focus.region.radius_m",
                radius_m,
                e,
            );
        }
    }
}

fn net(s: &Scenario, e: &mut Vec<ScenarioError>) {
    one_of("net.layer", &s.net.layer, &NET_LAYERS, e);
}

fn messages(s: &Scenario, e: &mut Vec<ScenarioError>) {
    // `messages.generator` tunes the generation timing and nothing else: the generation
    // rules themselves are the node runtime's (`v2xw_msg::generator`).
    if let Some(g) = &s.messages.generator {
        let known = [
            v2xw_msg::GENERATION_TIMING_ID,
            v2xw_msg::BSM_GENERATOR_ID,
            v2xw_msg::CAM_GENERATOR_ID,
        ];
        if !known.contains(&g.id.as_str()) {
            e.push(conflict(
                "messages.generator.id",
                format!(
                    "'{}' is not a generator this build ships; choose one of: {}",
                    g.id,
                    known.join(", ")
                ),
            ));
        }
        if let Some(obj) = g.params.as_object() {
            for (k, v) in obj {
                let limit = match k.as_str() {
                    "phase_window_ms" => 1_000.0,
                    "max_jitter_ms" => 100.0,
                    other => {
                        e.push(conflict(
                            &format!("messages.generator.params.{other}"),
                            "is not a generation-timing parameter; the two are \
                             phase_window_ms and max_jitter_ms"
                                .to_string(),
                        ));
                        continue;
                    }
                };
                match v.as_f64() {
                    Some(x) if x.is_finite() && (0.0..=limit).contains(&x) => {}
                    _ => e.push(conflict(
                        &format!("messages.generator.params.{k}"),
                        format!("is {v}, and it must be a number of milliseconds in [0, {limit}]"),
                    )),
                }
            }
        } else if !g.params.is_null() {
            e.push(conflict(
                "messages.generator.params",
                "must be an object of phase_window_ms and max_jitter_ms".to_string(),
            ));
        }
    }

    one_of(
        "messages.codec_tier",
        &s.messages.codec_tier,
        &CODEC_TIERS,
        e,
    );
    if s.messages.sets.is_empty() {
        e.push(conflict(
            "messages.sets",
            "is empty; a run with no message set generates no traffic, which is a scenario \
             with radio configured and nothing to carry — say so with actors.vehicles.\
             equipped_fraction: 0 instead"
                .to_string(),
        ));
    }
    let mut seen = BTreeSet::new();
    for (i, set) in s.messages.sets.iter().enumerate() {
        one_of(&format!("messages.sets[{i}]"), set, &MESSAGE_SETS, e);
        if !seen.insert(set.as_str()) {
            e.push(conflict(
                &format!("messages.sets[{i}]"),
                format!("'{set}' is listed twice, so its generator would be installed twice"),
            ));
        }
    }
    // D2: the hand-written J2735 codec covers the BSM. SPaT, MAP, PSM, SRM and SSM have a
    // validated size model and no real encoder, so `uper` cannot be honoured for them.
    if s.messages.codec_tier == "uper" {
        for (i, set) in s.messages.sets.iter().enumerate() {
            if matches!(set.as_str(), "spat" | "map" | "psm") {
                e.push(conflict(
                    &format!("messages.sets[{i}]"),
                    format!(
                        "'{set}' has no real UPER encoder (build decision D2: it ships as a \
                         validated size model), but messages.codec_tier is 'uper'; set \
                         codec_tier to 'size-model' or drop this set"
                    ),
                ));
            }
        }
    }
}

fn security(s: &Scenario, e: &mut Vec<ScenarioError>) {
    one_of("security.envelope", &s.security.envelope, &ENVELOPES, e);
    one_of(
        "security.verification_policy",
        &s.security.verification_policy,
        &POLICIES,
        e,
    );
    if s.security.crypto_mode == crate::scenario::schema::CryptoModeSpec::Real
        && !REAL_SIGNATURES.contains(&s.security.signature.as_str())
    {
        e.push(conflict(
            "security.signature",
            format!(
                "'{}' has no real implementation, but security.crypto_mode is 'real'; \
                 'modeled' costs any primitive the hardware profile prices, 'real' does the \
                 mathematics and only {} is implemented",
                s.security.signature,
                REAL_SIGNATURES.join(", ")
            ),
        ));
    }
    if s.security.signer_id_policy.full_cert_every_ms == 0
        && s.security.signer_id_policy.digest_otherwise
    {
        e.push(conflict(
            "security.signer_id_policy.full_cert_every_ms",
            "is 0 (attach the certificate on every message) while digest_otherwise is true; \
             the two say opposite things about what goes in the signer identifier"
                .to_string(),
        ));
    }
    let p = &s.security.pseudonym_change;
    match p.strategy.as_str() {
        "time" => {
            if p.period_s.is_none() {
                e.push(conflict(
                    "security.pseudonym_change.period_s",
                    "is missing, but security.pseudonym_change.strategy is 'time', which \
                     changes pseudonym on a period and has no default one"
                        .to_string(),
                ));
            }
        }
        "distance" => {
            if p.distance_m.is_none() {
                e.push(conflict(
                    "security.pseudonym_change.distance_m",
                    "is missing, but security.pseudonym_change.strategy is 'distance'".to_string(),
                ));
            }
        }
        "mix-zone" | "silent" => {}
        other => e.push(conflict(
            "security.pseudonym_change.strategy",
            format!(
                "'{other}' is not one this build implements; allowed: time, distance, \
                 mix-zone, silent"
            ),
        )),
    }
    if let Some(period) = p.period_s {
        bounded_at(
            "security.pseudonym_change.period_s",
            "security.pseudonym_change.period_s",
            period,
            e,
        );
    }
    if let Some(distance) = p.distance_m {
        bounded_at(
            "security.pseudonym_change.distance_m",
            "security.pseudonym_change.distance_m",
            distance,
            e,
        );
    }
}

fn nodes(s: &Scenario, e: &mut Vec<ScenarioError>) {
    if s.nodes.default_obu.trim().is_empty() {
        e.push(conflict(
            "nodes.default_obu",
            "is empty; every equipped vehicle runs on a hardware profile and there is no \
             default profile to fall back on, because a defaulted profile would set the \
             service times of a whole run invisibly (06-node-models.md §1)"
                .to_string(),
        ));
    }
    let obus = obu_profile_ids();
    if !s.nodes.default_obu.trim().is_empty() && !obus.contains(&s.nodes.default_obu.as_str()) {
        // Until this rule existed an unknown id fell through to the reference profile
        // without a word, so a scenario could name a device that does not ship and get a
        // different one's service times. The shipped default was itself such an id.
        e.push(conflict(
            "nodes.default_obu",
            format!(
                "'{}' is not a hardware profile this build ships, and an unknown profile \
                 silently becomes the reference on-board unit — which would set the \
                 service times of the whole run to a device the scenario did not name. \
                 Shipped on-board units: {}",
                s.nodes.default_obu,
                obus.join(", ")
            ),
        ));
    }
    for (name, profile) in &s.nodes.per_class {
        if !obus.contains(&profile.as_str()) {
            e.push(conflict(
                &format!("nodes.per_class.{name}"),
                format!(
                    "'{profile}' is not a hardware profile this build ships; shipped \
                     on-board units: {}",
                    obus.join(", ")
                ),
            ));
        }
    }
    for name in s.nodes.per_class.keys() {
        if !s.actors.vehicles.classes.is_empty() && !s.actors.vehicles.classes.contains_key(name) {
            e.push(conflict(
                &format!("nodes.per_class.{name}"),
                format!(
                    "names a vehicle class '{name}' that actors.vehicles.classes does not \
                     define (it defines {})",
                    s.actors
                        .vehicles
                        .classes
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
}

fn threats(s: &Scenario, e: &mut Vec<ScenarioError>) {
    if let Err(problems) = crate::run::jamming::jammer_specs(s) {
        for (path, why) in problems {
            e.push(conflict(&path, why));
        }
    }
    for (i, a) in s.threats.attackers.iter().enumerate() {
        let named = u8::from(a.fraction.is_some())
            + u8::from(a.count.is_some())
            + u8::from(!a.actor_ids.is_empty());
        if named != 1 {
            e.push(conflict(
                &format!("threats.attackers[{i}]"),
                format!(
                    "names {} of fraction, count and actor_ids; exactly one selects the \
                     attacker population, and two would need a rule for which wins",
                    if named == 0 {
                        "none".to_string()
                    } else {
                        named.to_string()
                    }
                ),
            ));
        }
        if let Some(f) = a.fraction {
            bounded_at(
                "threats.attackers[].fraction",
                &format!("threats.attackers[{i}].fraction"),
                f,
                e,
            );
        }
        if a.id.trim().is_empty() {
            e.push(conflict(
                &format!("threats.attackers[{i}].id"),
                "is empty; an attacker is a registered model and the id is how it is \
                 resolved and how the manifest pins it"
                    .to_string(),
            ));
        }
        if let Some(w) = a.schedule
            && (w.from_s < 0.0 || w.to_s > s.time.duration_s || w.to_s <= w.from_s)
        {
            e.push(conflict(
                &format!("threats.attackers[{i}].schedule"),
                format!(
                    "spans [{}, {}] s, which is not a non-empty interval inside the run \
                     [0, {}] s (time.duration_s)",
                    w.from_s, w.to_s, s.time.duration_s
                ),
            ));
        }
    }
}

fn metrics_and_exporters(s: &Scenario, e: &mut Vec<ScenarioError>) {
    if s.metrics.iter().any(|m| m == "all") && s.metrics.len() > 1 {
        e.push(conflict(
            "metrics",
            format!(
                "lists 'all' beside {} other entries; 'all' already selects every registered \
                 metric, so the list says two different things",
                s.metrics.len() - 1
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    for (i, x) in s.exporters.iter().enumerate() {
        if x.id.trim().is_empty() {
            e.push(conflict(
                &format!("exporters[{i}].id"),
                "is empty; an exporter is resolved by id".to_string(),
            ));
        } else if !seen.insert(x.id.as_str()) {
            e.push(conflict(
                &format!("exporters[{i}].id"),
                format!(
                    "'{}' is listed twice; the second would overwrite the first's output \
                     files",
                    x.id
                ),
            ));
        }
    }
}

fn timeline(s: &Scenario, e: &mut Vec<ScenarioError>) {
    let doc = match serde_json::to_value(s) {
        Ok(v) => v,
        // Unreachable for a `Scenario` (every field is plain data), so it is reported
        // rather than unwrapped: a panic in a validator is worse than a missed rule.
        Err(err) => {
            e.push(conflict(
                "events",
                format!("cannot be checked because the scenario does not serialise: {err}"),
            ));
            return;
        }
    };
    for (i, item) in s.events.iter().enumerate() {
        let field = format!("events[{i}]");
        if !(item.t.is_finite() && (0.0..=s.time.duration_s).contains(&item.t)) {
            e.push(conflict(
                &format!("{field}.t"),
                format!(
                    "is {} s, which is outside the run [0, {}] s (time.duration_s); an event \
                     past the horizon never fires",
                    item.t, s.time.duration_s
                ),
            ));
        }
        match item.until {
            Some(until) if !item.kind.takes_until() => e.push(conflict(
                &format!("{field}.until"),
                format!(
                    "is {until} s, but a '{}' has no end: it replaces the previous value \
                     rather than being undone",
                    serde_json::to_value(item.kind)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                ),
            )),
            Some(until) if until <= item.t || until > s.time.duration_s => e.push(conflict(
                &format!("{field}.until"),
                format!(
                    "is {until} s, which is not after t ({} s) and inside the run [0, {}] s",
                    item.t, s.time.duration_s
                ),
            )),
            _ => {}
        }
        for key in item.kind.required_params() {
            if !item.params.contains_key(*key) {
                e.push(conflict(
                    &format!("{field}.{key}"),
                    format!(
                        "is missing, and a '{}' event needs it",
                        serde_json::to_value(item.kind)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default()
                    ),
                ));
            }
        }
        if item.kind == TimelineKind::ParamChange
            && let Some(Value::String(path)) = item.params.get("path")
            && resolve_path(&doc, path).is_none()
        {
            e.push(conflict(
                &format!("{field}.path"),
                format!(
                    "'{path}' does not name a field of this scenario, so the change would \
                     be applied to nothing and the run would silently not do what the \
                     timeline says"
                ),
            ));
        }
        if item.kind == TimelineKind::WeatherFront
            && let Some(v) = item.params.get("value")
            && serde_json::from_value::<WeatherKind>(v.clone()).is_err()
        {
            e.push(conflict(
                &format!("{field}.value"),
                format!(
                    "{v} is not a weather kind; allowed: {}",
                    WeatherKind::ALL
                        .iter()
                        .filter_map(|k| serde_json::to_value(k).ok())
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        if item.kind == TimelineKind::DemandMultiplier
            && let Some(v) = item.params.get("value")
            && !v.as_f64().is_some_and(|x| x.is_finite() && x >= 0.0)
        {
            e.push(conflict(
                &format!("{field}.value"),
                format!("{v} is not a non-negative demand multiplier"),
            ));
        }
    }
}

fn experiment(s: &Scenario, e: &mut Vec<ScenarioError>) {
    let Some(x) = &s.experiment else { return };
    let doc = match serde_json::to_value(s) {
        Ok(v) => v,
        Err(_) => return,
    };
    if x.replications == 0 && x.seeds.is_empty() {
        e.push(conflict(
            "experiment.replications",
            "is 0 and experiment.seeds is empty, so the experiment has no runs in it".to_string(),
        ));
    }
    for (path, values) in &x.sweep {
        if resolve_path(&doc, path).is_none() {
            e.push(conflict(
                &format!("experiment.sweep.{path}"),
                format!("'{path}' does not name a field of this scenario"),
            ));
        }
        if values.is_empty() {
            e.push(conflict(
                &format!("experiment.sweep.{path}"),
                "has no values, so the sweep over it is empty and the whole experiment \
                 collapses to nothing"
                    .to_string(),
            ));
        }
    }
}

/// Walks a dotted path into a document, `a.b[2].c`.
///
/// Used by two rules — `param.change` paths and sweep paths — which is the whole reason
/// the scenario is re-serialised during validation: a path is checked against the
/// document the author wrote, not against a list of paths kept in step by hand.
pub fn resolve_path<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = doc;
    for segment in path.split('.') {
        let (name, indices) = split_indices(segment)?;
        if !name.is_empty() {
            cur = cur.get(name)?;
        }
        for i in indices {
            cur = cur.get(i)?;
        }
    }
    Some(cur)
}

/// `foo[1][2]` into `("foo", [1, 2])`; `None` if the brackets are malformed.
fn split_indices(segment: &str) -> Option<(&str, Vec<usize>)> {
    let Some(open) = segment.find('[') else {
        return Some((segment, Vec::new()));
    };
    let (name, rest) = segment.split_at(open);
    let mut indices = Vec::new();
    let mut rest = rest;
    while !rest.is_empty() {
        let close = rest.find(']')?;
        if !rest.starts_with('[') {
            return None;
        }
        indices.push(rest[1..close].parse::<usize>().ok()?);
        rest = &rest[close + 1..];
    }
    Some((name, indices))
}
