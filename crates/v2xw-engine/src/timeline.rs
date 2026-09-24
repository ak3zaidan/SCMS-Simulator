//! The scenario timeline: what each event kind does, and what may be changed mid-run.
//!
//! 03-interfaces.md §13 gives a scenario a list of `{t, until?, type, params}` items, and
//! the kernel schedules each as a control-priority event (`EventClass::Control`, priority
//! 0), so everything that happens at that instant — a mobility step, a node phase, a
//! frame — sees the change. This module holds the parts of each kind that do not need the
//! running engine: parsing a closure's target, the table of parameters a `param.change` may
//! touch and how far each reaches, the demand multiplier's peak (which the thinned Poisson
//! process needs up front), and the window an `attack.wave` gives each attacker population.
//!
//! # What each kind does
//!
//! | Kind | Effect | Ends with `until` |
//! |---|---|---|
//! | `weather.front` | the weather every driver and every radio link sees | back to `weather.initial` |
//! | `outage` | node `target` stops transmitting and receiving | the node comes back |
//! | `demand.multiplier` | the thinned-Poisson arrival rate is multiplied by `value` | the multiplier is lifted |
//! | `closure` | the lanes `target` names cost infinity to every router; vehicles re-plan | the lanes reopen |
//! | `param.change` | one parameter from [`LIVE_PARAMS`] takes a new value | — (a later change replaces it) |
//! | `attack.wave` | the attacker populations `ids` act only inside `[t, until)` | the populations go quiet |
//!
//! Several multipliers active at once compose by product, and with a `param.change` of
//! `actors.vehicles.demand.rate_veh_per_h`, which is expressed as the ratio of the new rate
//! to the scenario's own.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use v2xw_core::ids::{EdgeId, LaneId};
use v2xw_world::World;
use v2xw_world::model::LaneKind;

use crate::scenario::schema::{Scenario, TimelineKind};

/// What a closure's `target` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureTarget {
    /// One lane, by id: `"lane:123"` or the bare number `123`.
    Lane(u32),
    /// Every lane of one edge, by id: `"edge:45"`.
    Edge(u32),
    /// Every edge whose street name is this one, ignoring case: `"street:West 42nd Street"`.
    Street(String),
}

impl ClosureTarget {
    /// Parses a `target` value.
    ///
    /// # Errors
    /// A sentence saying what the accepted spellings are.
    pub fn parse(value: &Value) -> Result<Self, String> {
        const HINT: &str = "write a lane id, \"lane:<id>\", \"edge:<id>\" or \"street:<name>\"";
        if let Some(n) = value.as_u64() {
            return u32::try_from(n)
                .map(ClosureTarget::Lane)
                .map_err(|_| format!("{n} is not a lane id; {HINT}"));
        }
        let Some(text) = value.as_str() else {
            return Err(format!("{value} is not a closure target; {HINT}"));
        };
        let (kind, rest) = text.split_once(':').unwrap_or(("lane", text));
        let rest = rest.trim();
        let id = || {
            rest.parse::<u32>()
                .map_err(|_| format!("'{rest}' in '{text}' is not a number; {HINT}"))
        };
        match kind.trim() {
            "lane" => id().map(ClosureTarget::Lane),
            "edge" => id().map(ClosureTarget::Edge),
            "street" if !rest.is_empty() => Ok(ClosureTarget::Street(rest.to_string())),
            _ => Err(format!("'{text}' is not a closure target; {HINT}")),
        }
    }

    /// The lanes this target closes in `world`, ascending: the vehicle lanes of what it
    /// names. Footways and marked crossings stay open — a road closure closes the road,
    /// and pedestrians route on the sidewalk graph.
    pub fn lanes(&self, world: &World) -> Vec<LaneId> {
        let vehicular = |kind: LaneKind| !matches!(kind, LaneKind::Sidewalk | LaneKind::Crossing);
        let mut out: BTreeSet<LaneId> = BTreeSet::new();
        match self {
            ClosureTarget::Lane(id) => {
                if let Some(lane) = world.try_lane(LaneId::new(*id))
                    && vehicular(lane.kind)
                {
                    out.insert(lane.id);
                }
            }
            ClosureTarget::Edge(id) => {
                if let Some(edge) = world.roads.try_edge(EdgeId::new(*id)) {
                    for lane in &edge.lanes {
                        if world.try_lane(*lane).is_some_and(|l| vehicular(l.kind)) {
                            out.insert(*lane);
                        }
                    }
                }
            }
            ClosureTarget::Street(name) => {
                let wanted = name.to_lowercase();
                for edge in world.roads.edges() {
                    let named = world.symbols.resolve_optional(edge.name);
                    if named.is_empty() || named.to_lowercase() != wanted {
                        continue;
                    }
                    for lane in &edge.lanes {
                        if world.try_lane(*lane).is_some_and(|l| vehicular(l.kind)) {
                            out.insert(*lane);
                        }
                    }
                }
            }
        }
        out.into_iter().collect()
    }
}

/// How far a live parameter change reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Everything, from the instant of the change.
    Now,
    /// Every vehicle or device that enters the run after the change; those already in it
    /// keep what they were built with, as a fleet does when a policy changes.
    NewArrivals,
}

impl Reach {
    /// The word the settings page and the run log use.
    pub const fn label(self) -> &'static str {
        match self {
            Reach::Now => "now",
            Reach::NewArrivals => "new-arrivals",
        }
    }
}

/// A parameter a `param.change` may set during a run.
#[derive(Debug, Clone, Copy)]
pub struct LiveParam {
    /// The dotted path, or the prefix every accepted path starts with when it ends in `.`.
    pub path: &'static str,
    /// How far the change reaches.
    pub reach: Reach,
    /// What it does, for the settings page and the error that refuses anything else.
    pub note: &'static str,
}

/// Every parameter a `param.change` may set, and nothing else.
///
/// A parameter that is read once when the kernel is built — the world, the seed, the
/// radio technology, a model choice — cannot honestly change mid-run: the model that read
/// it is already built. Those are refused by name at load rather than accepted and ignored.
pub const LIVE_PARAMS: &[LiveParam] = &[
    LiveParam {
        path: "weather.",
        reach: Reach::Now,
        note: "The weather every driver and radio link sees (initial, intensity, \
               visibility_m, surface), as a weather front sets it.",
    },
    LiveParam {
        path: "actors.vehicles.demand.rate_veh_per_h",
        reach: Reach::Now,
        note: "The thinned-Poisson arrival rate; applied as the ratio to the scenario's own \
               rate, which the candidate process was sized for.",
    },
    LiveParam {
        path: "actors.vehicles.equipped_fraction",
        reach: Reach::NewArrivals,
        note: "The share of vehicles that carry an OBU, for vehicles that enter after the \
               change.",
    },
    LiveParam {
        path: "actors.vru.device_fraction",
        reach: Reach::NewArrivals,
        note: "The share of pedestrians and cyclists that carry a device, for those that \
               enter after the change.",
    },
    LiveParam {
        path: "security.verification_policy",
        reach: Reach::NewArrivals,
        note: "Which received messages a node verifies, for nodes that enter after the \
               change.",
    },
    LiveParam {
        path: "security.pseudonym_change.",
        reach: Reach::NewArrivals,
        note: "The pseudonym-rotation rule of OBUs that enter after the change.",
    },
    LiveParam {
        path: "nodes.default_obu",
        reach: Reach::NewArrivals,
        note: "The hardware profile of OBUs that enter after the change.",
    },
];

/// The live-parameter row for `path`, if a `param.change` may set it.
pub fn live_param(path: &str) -> Option<&'static LiveParam> {
    LIVE_PARAMS.iter().find(|p| {
        if let Some(prefix) = p.path.strip_suffix('.') {
            path.strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('.'))
                .is_some_and(|rest| !rest.is_empty())
        } else {
            p.path == path
        }
    })
}

/// The accepted paths, for an error message.
pub fn live_param_list() -> String {
    LIVE_PARAMS
        .iter()
        .map(|p| {
            if p.path.ends_with('.') {
                format!("{}*", p.path)
            } else {
                p.path.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `scenario` with `path` set to `value`, through its JSON form.
///
/// # Errors
/// A sentence when the path cannot be written or the result is not a scenario.
pub fn with_param(scenario: &Scenario, path: &str, value: &Value) -> Result<Scenario, String> {
    let mut doc = serde_json::to_value(scenario).map_err(|e| e.to_string())?;
    let mut cur = &mut doc;
    let segments: Vec<&str> = path.split('.').collect();
    for (i, segment) in segments.iter().enumerate() {
        let Some(map) = cur.as_object_mut() else {
            return Err(format!(
                "'{path}' goes through a value that is not an object"
            ));
        };
        if i + 1 == segments.len() {
            map.insert((*segment).to_string(), value.clone());
            break;
        }
        cur = map
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
    serde_json::from_value(doc).map_err(|e| format!("{value} does not fit '{path}': {e}"))
}

/// The scenario's own vehicle arrival rate, veh/h: what a rate change is a ratio to.
pub fn base_rate_veh_per_h(scenario: &Scenario) -> Option<f64> {
    let d = &scenario.actors.vehicles.demand;
    d.rate_veh_per_h.or_else(|| {
        d.params
            .get("arrival_rate_per_s")
            .and_then(Value::as_f64)
            .map(|r| r * 3600.0)
    })
}

/// The largest demand multiplier the timeline ever puts in force at once.
///
/// The thinned Poisson process draws candidates at `rate · boost` and keeps each with
/// probability `m(t) / boost`, which is exact only while `boost ≥ m(t)`. So the boost has
/// to be known before the first draw, and this walks the timeline to find it: at every
/// instant a multiplier starts or ends, or a rate change lands, it recomputes the product
/// in force. With no demand event it is `1`, and the draw sequence is the one a scenario
/// without a timeline has.
pub fn demand_peak(scenario: &Scenario) -> f64 {
    let base = base_rate_veh_per_h(scenario).filter(|r| *r > 0.0);
    // (time, order, change). Ends sort before starts at the same instant, so a back-to-back
    // pair does not count as overlapping.
    let mut changes: Vec<(f64, u8, usize)> = Vec::new();
    for (i, item) in scenario.events.iter().enumerate() {
        match item.kind {
            TimelineKind::DemandMultiplier => {
                changes.push((item.t, 1, i));
                if let Some(until) = item.until {
                    changes.push((until, 0, i));
                }
            }
            TimelineKind::ParamChange
                if item.params.get("path").and_then(Value::as_str)
                    == Some("actors.vehicles.demand.rate_veh_per_h") =>
            {
                changes.push((item.t, 1, i));
            }
            _ => {}
        }
    }
    changes.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut active: BTreeMap<usize, f64> = BTreeMap::new();
    let mut rate_ratio = 1.0_f64;
    let mut peak = 1.0_f64;
    for (_, order, i) in changes {
        let item = &scenario.events[i];
        let value = item
            .params
            .get("value")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);
        match item.kind {
            TimelineKind::DemandMultiplier if order == 1 => {
                active.insert(i, value);
            }
            TimelineKind::DemandMultiplier => {
                active.remove(&i);
            }
            _ => {
                if let Some(base) = base {
                    rate_ratio = value / base;
                }
            }
        }
        let now: f64 = rate_ratio * active.values().product::<f64>();
        if now.is_finite() {
            peak = peak.max(now);
        }
    }
    peak
}

/// The window each attacker population acts in when an `attack.wave` names it, by its
/// index in `threats.attackers`: `(from_s, to_s)`, `to_s` the run's end when the wave has
/// no `until`.
///
/// A population no wave names keeps its own `schedule`. A population a wave names is quiet
/// outside the wave: the wave *is* its schedule.
pub fn attack_windows(scenario: &Scenario) -> BTreeMap<usize, (f64, f64)> {
    let mut out = BTreeMap::new();
    for item in &scenario.events {
        if item.kind != TimelineKind::AttackWave {
            continue;
        }
        let to = item.until.unwrap_or(scenario.time.duration_s);
        for index in wave_populations(scenario, item.params.get("ids")).unwrap_or_default() {
            out.insert(index, (item.t, to));
        }
    }
    out
}

/// The attacker populations an `attack.wave`'s `ids` names, by index into
/// `threats.attackers`. An id is either that index or a population's model id, which names
/// every population with that id.
///
/// # Errors
/// A sentence naming the id that matches no population.
pub fn wave_populations(scenario: &Scenario, ids: Option<&Value>) -> Result<Vec<usize>, String> {
    let attackers = &scenario.threats.attackers;
    let list: Vec<Value> = match ids {
        Some(Value::Array(a)) => a.clone(),
        Some(v @ (Value::String(_) | Value::Number(_))) => vec![v.clone()],
        _ => return Err("must be a list of attacker populations".to_string()),
    };
    if list.is_empty() {
        return Err("names no attacker population".to_string());
    }
    let mut out = BTreeSet::new();
    for id in &list {
        let found: Vec<usize> = match id {
            Value::Number(n) => n
                .as_u64()
                .and_then(|i| usize::try_from(i).ok())
                .filter(|i| *i < attackers.len())
                .into_iter()
                .collect(),
            Value::String(s) => attackers
                .iter()
                .enumerate()
                .filter(|(_, a)| a.id == *s)
                .map(|(i, _)| i)
                .collect(),
            _ => Vec::new(),
        };
        if found.is_empty() {
            let known: Vec<String> = attackers
                .iter()
                .enumerate()
                .map(|(i, a)| format!("{i} ({})", a.id))
                .collect();
            return Err(format!(
                "{id} is not an attacker population of this scenario; threats.attackers has {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
        }
        out.extend(found);
    }
    Ok(out.into_iter().collect())
}
