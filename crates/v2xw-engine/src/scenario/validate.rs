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

/// Every problem with `s`, in schema order. Empty means the scenario is loadable.
pub fn validate(s: &Scenario) -> Vec<ScenarioError> {
    let mut e = Vec::new();
    time(s, &mut e);
    world(s, &mut e);
    actors(s, &mut e);
    radio(s, &mut e);
    net(s, &mut e);
    messages(s, &mut e);
    security(s, &mut e);
    nodes(s, &mut e);
    threats(s, &mut e);
    metrics_and_exporters(s, &mut e);
    timeline(s, &mut e);
    experiment(s, &mut e);
    e
}

fn conflict(field: &str, conflict: String) -> ScenarioError {
    ScenarioError::conflict(field, conflict)
}

/// `value` is in `[lo, hi]`, or an error saying so.
fn in_range(field: &str, what: &str, value: f64, lo: f64, hi: f64, e: &mut Vec<ScenarioError>) {
    if !value.is_finite() || value < lo || value > hi {
        e.push(conflict(
            field,
            format!("{what} is {value}, which is outside the allowed range [{lo}, {hi}]"),
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
    if !(s.time.duration_s.is_finite() && s.time.duration_s > 0.0) {
        e.push(conflict(
            "time.duration_s",
            format!(
                "is {}, and a run has to last a positive number of seconds",
                s.time.duration_s
            ),
        ));
    }
    // ADR 0004 decision 2: 10–100 ms, SUMO tier ≥ 10 ms.
    if !(10..=100).contains(&s.time.mobility_step_ms) {
        e.push(conflict(
            "time.mobility_step_ms",
            format!(
                "is {}, and ADR 0004 decision 2 allows 10–100 ms: below 10 ms no mobility \
                 provider is calibrated for the step, and above 100 ms the published \
                 constant-velocity extrapolation between steps stops being accurate enough \
                 for frame-level radio",
                s.time.mobility_step_ms
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
        in_range(
            "world.buildings.metres_per_level",
            "the storey height",
            mpl,
            1.5,
            10.0,
            e,
        );
    }
}

fn actors(s: &Scenario, e: &mut Vec<ScenarioError>) {
    in_range(
        "actors.vehicles.equipped_fraction",
        "the equipped fraction",
        s.actors.vehicles.equipped_fraction,
        0.0,
        1.0,
        e,
    );
    in_range(
        "actors.vru.device_fraction",
        "the device fraction",
        s.actors.vru.device_fraction,
        0.0,
        1.0,
        e,
    );

    if !s.actors.vehicles.classes.is_empty() {
        let mut total = 0.0;
        for (name, c) in &s.actors.vehicles.classes {
            in_range(
                &format!("actors.vehicles.classes.{name}.fraction"),
                "the class share",
                c.fraction,
                0.0,
                1.0,
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

    let mut seen_sites = BTreeSet::new();
    for (i, r) in s.actors.rsus.iter().enumerate() {
        if !seen_sites.insert(r.site) {
            e.push(conflict(
                &format!("actors.rsus[{i}].site"),
                format!(
                    "site {} already carries a roadside unit; two units at one site would \
                     share a position and the node ids would be assigned by list order",
                    r.site
                ),
            ));
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

fn radio(s: &Scenario, e: &mut Vec<ScenarioError>) {
    let t = &s.radio.tiers;

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
            && !(radius_m.is_finite() && radius_m > 0.0)
        {
            e.push(conflict(
                "radio.tiers.focus.region.radius_m",
                format!("is {radius_m}, and a follow radius is a positive number of metres"),
            ));
        }
    }
}

fn net(s: &Scenario, e: &mut Vec<ScenarioError>) {
    one_of("net.layer", &s.net.layer, &NET_LAYERS, e);
}

fn messages(s: &Scenario, e: &mut Vec<ScenarioError>) {
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
        in_range(
            "security.pseudonym_change.period_s",
            "the period",
            period,
            1.0,
            86_400.0,
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
            in_range(
                &format!("threats.attackers[{i}].fraction"),
                "the attacker fraction",
                f,
                0.0,
                1.0,
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
