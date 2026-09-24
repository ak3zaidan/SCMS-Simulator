//! Composing the nine crates into one run.
//!
//! Every choice here is made from the scenario and nothing else, and every model that ends
//! up in a run is registered first, so the manifest pins it (02-architecture.md §6.5). A
//! model the scenario does not name is not silently defaulted to "something reasonable":
//! the default is stated here, in one place, with its citation, and it is registered
//! exactly like a named one so it appears in the manifest under its own id.
//!
//! # The credential bootstrap
//!
//! 03-interfaces.md §7 puts enrolment and top-up in a `CredentialProtocol` plug-in, and
//! `v2xw-node`'s own documentation says no such protocol ships yet. A node with no
//! credential cannot sign, and `ObuRuntime::generate` correctly refuses to transmit rather
//! than sending unsigned — so a run with no bootstrap produces no traffic at all.
//!
//! [`bootstrap_credentials`] is therefore explicit about what it is: a **stand-in for the
//! protocol**, not a model of it. It installs one pseudonym per node with the scenario's
//! rotation period, derived from the node id through `v2xw_node::stores::pseudo_signer`,
//! and it does not model a request, a batch, a top-up latency or a provisioning failure.
//! A scenario measuring provisioning must not use it, which is why the run report counts
//! the credentials it installed.

use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::Dims;
use v2xw_core::ids::NodeId;
use v2xw_core::registry::Registry;
use v2xw_core::time::{Duration, SimTime, WallClock};
use v2xw_core::weather::{SurfaceCondition, WeatherKind, WeatherState};
use v2xw_mobility::{
    Demand, GnssModel, Mobility, NativeMobility, NoDemand, PoissonDemand, VehicleClass,
};
use v2xw_msg::cam::ParticipantType;
use v2xw_node::stores::{CredState, CredentialHandle, RotationPolicy, pseudo_signer};
use v2xw_node::{
    CryptoMode, NodeConfig, ObuRuntime, OnDemand, Prioritized, ServiceSet, VerificationPolicy,
    VerifyAll,
};
use v2xw_sec::envelope::{EnvelopeProfile, SignerIdPolicy};
use v2xw_world::{ImportOptions, World, WorldSource, WorldSourceSpec};

use crate::adapters::{BoxedFading, BoxedPropagation};
use crate::error::{EngineError, Result};
use crate::scenario::Scenario;

/// The transmit power every OBU uses, dBm.
///
/// 20 dBm EIRP is the SAE J2945/1 congestion-controlled default for a Class B device and
/// the figure `NodeConfig::default()` already carries; it is named here so the link budget
/// and the node configuration cannot drift apart.
pub const TX_POWER_DBM: f64 = 20.0;

/// Builds the world the scenario names.
///
/// # Errors
/// [`EngineError::World`] if the source cannot be built or imported.
pub fn build_world(scenario: &Scenario) -> Result<World> {
    match scenario.world.cache.as_deref().map(str::trim) {
        Some(dir) if !dir.is_empty() => cached_world(scenario, std::path::Path::new(dir)),
        _ => import_world(scenario),
    }
}

/// The importers' output revision, part of every [`world_cache_key`].
///
/// The workspace version never moves (it is `0.1.0` for every commit), so it cannot tell a
/// cache written by an older importer from one written by this one. Bump this whenever an
/// importer's output changes for the same inputs, or a kept cache entry replays the old
/// world. Revision 2: the traffic track's OSM connector, stop-line setback, tunnel/bridge
/// height and lane-pairing fixes changed the Manhattan world's content hash. Revision 3:
/// the junction track's rounded lane corners and arc connectors, fork lane assignment,
/// passage lanes and ITE change intervals changed it again.
pub const IMPORTER_REVISION: u32 = 3;

/// The key a world is cached under: a digest of everything that decides what the import
/// produces — the scenario's `world` section (less `cache` itself), the bytes of the source
/// file and of the terrain raster when the scenario names them, and the importer's revision
/// ([`IMPORTER_REVISION`]).
///
/// The source file's *content* is hashed, not its name or its modification time, so an
/// edited extract is a different world and a copied one is the same world. Hashing a large
/// extract costs a fraction of importing it, which is the whole trade.
///
/// # Errors
/// [`EngineError::Io`] if the source file cannot be read.
pub fn world_cache_key(scenario: &Scenario) -> Result<String> {
    let mut section = serde_json::to_value(&scenario.world).map_err(|e| {
        EngineError::Scenario(crate::ScenarioError::conflict("world", e.to_string()))
    })?;
    if let Some(map) = section.as_object_mut() {
        map.remove("cache");
    }
    let canonical = v2xw_core::hash::canonical_json(&section).map_err(|e| {
        EngineError::Scenario(crate::ScenarioError::conflict("world", e.to_string()))
    })?;
    let mut material = format!(
        "v2xw-world-cache/1 importer={IMPORTER_REVISION} workspace={} native=1\n",
        env!("CARGO_PKG_VERSION")
    )
    .into_bytes();
    material.extend_from_slice(&canonical);
    if let Some(path) = section
        .get("source")
        .and_then(|s| s.get("path"))
        .and_then(serde_json::Value::as_str)
    {
        let bytes = std::fs::read(path).map_err(|e| EngineError::Io {
            path: path.to_string(),
            source: e,
        })?;
        material.extend_from_slice(b"\nsource-sha256=");
        material.extend_from_slice(v2xw_core::hash::sha256_hex(&bytes).as_bytes());
    }
    // The terrain raster is an input of the import too (`attach_terrain`): an edited DEM
    // under the same name is a different world.
    if let Some(path) = section
        .get("terrain")
        .and_then(|t| t.get("dem"))
        .and_then(serde_json::Value::as_str)
    {
        let bytes = std::fs::read(path).map_err(|e| EngineError::Io {
            path: path.to_string(),
            source: e,
        })?;
        material.extend_from_slice(b"\nterrain-sha256=");
        material.extend_from_slice(v2xw_core::hash::sha256_hex(&bytes).as_bytes());
    }
    Ok(v2xw_core::hash::sha256_hex(&material))
}

/// `world.cache`: read the world from the cache directory when it is there, import it and
/// write it there when it is not.
///
/// The cached form is `v2xw_world::serde_native`, whose round trip is exact — same content
/// hash, same lane graph, same conflict matrices — so a cached run is the same run, and the
/// determinism contract is untouched. An unreadable or corrupt cache entry is not an error:
/// the world is imported again and the entry rewritten, because a cache must never be the
/// reason a run does not start. The write goes to a temporary name and is renamed into
/// place, so two runs filling the same entry cannot leave half a file behind.
fn cached_world(scenario: &Scenario, dir: &std::path::Path) -> Result<World> {
    let key = world_cache_key(scenario)?;
    let entry = dir.join(format!("{key}.v2xwworld"));
    if let Ok(bytes) = std::fs::read(&entry)
        && let Ok(world) = v2xw_world::serde_native::from_bytes(&bytes)
    {
        return Ok(world);
    }
    let world = import_world(scenario)?;
    if std::fs::create_dir_all(dir).is_ok()
        && let Ok(bytes) = v2xw_world::serde_native::to_bytes(&world)
    {
        let partial = dir.join(format!("{key}.{}.partial", std::process::id()));
        if std::fs::write(&partial, &bytes).is_ok() && std::fs::rename(&partial, &entry).is_err() {
            let _ = std::fs::remove_file(&partial);
        }
    }
    Ok(world)
}

/// Imports or generates the world the scenario names and attaches its terrain, with no
/// cache.
fn import_world(scenario: &Scenario) -> Result<World> {
    let world = build_world_geometry(scenario)?;
    attach_terrain(scenario, world)
}

/// Reads `world.terrain.dem`, when the scenario names one, and attaches it to the world.
///
/// The DEM is resampled onto the world's own frame and extent
/// ([`v2xw_world::dem::import_terrain_for_world`]) and attached **without draping**
/// ([`v2xw_world::DrapeOptions::none`]): lanes, junctions and buildings keep the `z` the
/// importer gave them, and the terrain is what the radio's knife-edge diffraction reads as
/// the ground between two antennas (`obstacle/terrain/knife-edge-p526`, composed by
/// [`build_obstacles`]). An SRTM `.hgt` tile or an ESRI ASCII grid in geographic
/// coordinates is read; the world's content hash then covers the grid.
///
/// # Errors
/// [`EngineError::World`] if the file cannot be read or does not cover a usable grid.
fn attach_terrain(scenario: &Scenario, world: World) -> Result<World> {
    let Some(path) = scenario.world.terrain.dem.as_deref() else {
        return Ok(world);
    };
    let (terrain, report) = v2xw_world::dem::import_terrain_for_world(
        &world,
        path,
        &v2xw_world::DemOptions::default(),
    )?;
    let (world, _drape) =
        v2xw_world::dem::with_terrain(&world, terrain, &v2xw_world::DrapeOptions::none(), &report)?;
    Ok(world)
}

fn build_world_geometry(scenario: &Scenario) -> Result<World> {
    let opts = ImportOptions::default().imported_at(scenario.world.imported_at.clone());
    let opts = ImportOptions {
        keep_building_holes: scenario.world.buildings.keep_holes,
        metres_per_level: scenario
            .world
            .buildings
            .metres_per_level
            .unwrap_or(opts.metres_per_level),
        ..opts
    };
    match &scenario.world.source {
        WorldSourceSpec::Procedural { params, .. } => {
            let grid: v2xw_world::procedural::GridParams = if params.is_null() {
                v2xw_world::procedural::GridParams::legacy()
            } else {
                serde_json::from_value(params.clone()).map_err(|e| {
                    EngineError::Scenario(crate::ScenarioError::conflict(
                        "world.source.params",
                        format!("does not fit the procedural grid generator's parameters: {e}"),
                    ))
                })?
            };
            Ok(v2xw_world::procedural::grid(&grid, &opts)?)
        }
        spec @ WorldSourceSpec::OsmXml { bbox, .. } => {
            // The importer has no default class-default preset on purpose: a fallback
            // speed limit is a jurisdictional fact, so the scenario must state it. Refuse
            // with the field name rather than guessing, which is what the importer itself
            // would do one layer down.
            let preset = scenario.world.highway_preset.ok_or_else(|| {
                EngineError::Scenario(crate::ScenarioError::conflict(
                    "world.highway_preset",
                    "an osm-xml world needs an explicit highway=* class-default preset, \
                     because a fallback speed limit is a statement about a jurisdiction; \
                     select one of: sumo-german, urban-us-nyc",
                ))
            })?;
            let mut osm = v2xw_world::osm::OsmOptions {
                import: opts.clone(),
                ..Default::default()
            }
            .highway_preset(preset);
            if let Some(b) = bbox {
                osm = osm.bbox(*b);
            }
            let source = v2xw_world::osm::OsmSource::with_options(osm);
            Ok(source.build(spec, &opts)?)
        }
        other => {
            // Any remaining source is another importer's. `GridSource` refuses what it
            // does not know rather than pretending, which is the error the caller sees.
            let source = v2xw_world::procedural::GridSource::new();
            Ok(source.build(other, &opts)?)
        }
    }
}

/// Registers every model a run can select, so the manifest pins all of them.
///
/// # Errors
/// [`EngineError::Registry`] if a card fails validation or two models share an id.
pub fn register_all(registry: &mut Registry) -> Result<()> {
    v2xw_node::register_all(registry)?;
    // The message generators, the two network layers and the fragmenters: the models the
    // `messages.generator`, `net.layer` and `net.fragmenter` keys choose between. Without
    // them the page's choice lists for those keys were empty and the manifest pinned none
    // of the models that framed and paced every frame of the run.
    let extra: Vec<v2xw_core::card::ModelCard> = vec![
        v2xw_core::model::Model::card(&v2xw_msg::generator::BsmGenerator::default()).clone(),
        v2xw_core::model::Model::card(&v2xw_msg::generator::CamGenerator::default()).clone(),
        v2xw_core::model::Model::card(&v2xw_net::WsmpNetLayer::default()).clone(),
        v2xw_core::model::Model::card(&v2xw_net::GnBtpNetLayer::default()).clone(),
        v2xw_core::model::Model::card(&v2xw_net::NoneFragmenter::default()).clone(),
    ];
    for card in extra {
        if !registry.contains(&card.id) {
            registry.register(card)?;
        }
    }
    for (_, card) in v2xw_mobility::model_cards() {
        // A card already registered by another crate is not an error here: two crates may
        // legitimately publish the same model. `register` refuses a *different* card under
        // the same id, which is the case worth failing on.
        if !registry.contains(&card.id) {
            registry.register(card.clone())?;
        }
    }
    Ok(())
}

/// The mobility provider the scenario names.
pub fn build_mobility(scenario: &Scenario) -> Box<dyn Mobility> {
    Box::new(native_mobility(scenario))
}

/// The native mobility engine exactly as [`build_mobility`] configures it, as its concrete
/// type — what the traffic-invariant auditor (`examples/traffic_audit.rs`) steps, so the
/// run it audits is the run the kernel would drive.
pub fn native_mobility(scenario: &Scenario) -> NativeMobility {
    let params = v2xw_mobility::EngineParams {
        step: scenario.time.mobility_step(),
        ..v2xw_mobility::EngineParams::default()
    };
    NativeMobility::new(params).with_vru_population(v2xw_mobility::engine::VruPopulation {
        pedestrians: scenario.actors.vru.pedestrians,
        cyclists: scenario.actors.vru.cyclists,
    })
}

/// True for the classes `actors.vru` populates: whether an actor carries a device is drawn
/// with `actors.vru.device_fraction` for these and `actors.vehicles.equipped_fraction` for
/// every other class.
pub const fn is_vru_class(class: VehicleClass) -> bool {
    matches!(class, VehicleClass::Pedestrian | VehicleClass::Bicycle)
}

/// The demand models a scenario may name in `actors.vehicles.demand.kind`.
///
/// `mobility/demand/poisson` is the scenario spelling of the thinned-Poisson model, whose
/// card id is `mobility/demand/poisson-thinned`; both are accepted.
pub const DEMAND_KINDS: [&str; 4] = [
    "mobility/demand/none",
    "mobility/demand/poisson",
    v2xw_mobility::demand::poisson::MODEL_ID,
    v2xw_mobility::demand::tr36885::MODEL_ID,
];

/// The vehicle classes `actors.vehicles.classes` may name: the motorised ones. A cyclist
/// or a pedestrian is a vulnerable road user and belongs in `actors.vru`.
pub fn vehicle_class_named(name: &str) -> Option<VehicleClass> {
    VehicleClass::ALL
        .into_iter()
        .find(|c| c.as_str() == name)
        .filter(|c| !matches!(c, VehicleClass::Bicycle | VehicleClass::Pedestrian))
}

/// The demand model the scenario names.
///
/// * `mobility/demand/none` — nothing arrives.
/// * `mobility/demand/poisson` (or `…/poisson-thinned`) — the thinned-Poisson model;
///   `params` is its own parameter struct plus an optional `od` object for the
///   origin-destination law ([`v2xw_mobility::demand::OdParams`]).
/// * `mobility/demand/tr36885-drop` — the 3GPP TR 36.885 vehicle drop; `params` is its
///   [`v2xw_mobility::demand::DropParams`].
///
/// `actors.vehicles.classes`, when given, is the fleet mix: its shares replace the Poisson
/// model's `fleet` preset, and a single class sets the drop model's class.
///
/// # Errors
/// [`EngineError::Scenario`] if `params` does not fit the model, and
/// [`EngineError::Mobility`] if the world admits no trip the demand model could place.
pub fn build_demand(scenario: &Scenario, world: &World) -> Result<Box<dyn Demand>> {
    let d = &scenario.actors.vehicles.demand;
    let bad = |what: &str, e: String| {
        EngineError::Scenario(crate::ScenarioError::conflict(
            "actors.vehicles.demand.params",
            format!("does not fit the {what}'s parameters: {e}"),
        ))
    };
    let shares: Vec<(VehicleClass, f64)> = scenario
        .actors
        .vehicles
        .classes
        .iter()
        .filter_map(|(name, c)| vehicle_class_named(name).map(|class| (class, c.fraction)))
        .collect();
    match d.kind.as_str() {
        "mobility/demand/none" => Ok(Box::new(NoDemand::new())),
        k if k == v2xw_mobility::demand::tr36885::MODEL_ID => {
            let mut params: v2xw_mobility::demand::DropParams = if d.params.is_null() {
                v2xw_mobility::demand::DropParams::default()
            } else {
                serde_json::from_value(d.params.clone())
                    .map_err(|e| bad("TR 36.885 drop model", e.to_string()))?
            };
            if let [(class, _)] = shares.as_slice() {
                params.class = *class;
            }
            Ok(Box::new(v2xw_mobility::demand::DropModel::new(params)))
        }
        _ => {
            // `params` is the model's own parameter struct, so a scenario reaches every
            // field the demand model publishes — including `max_total_vehicles`, the only
            // way to ask for an exact fleet size — plus `od` for the OD law.
            let mut raw = if d.params.is_null() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                d.params.clone()
            };
            let od: v2xw_mobility::demand::OdParams =
                match raw.as_object_mut().and_then(|m| m.remove("od")) {
                    Some(v) => serde_json::from_value(v)
                        .map_err(|e| bad("origin-destination law", e.to_string()))?,
                    None => v2xw_mobility::demand::OdParams::default(),
                };
            let mut params: v2xw_mobility::demand::PoissonParams = serde_json::from_value(raw)
                .map_err(|e| bad("thinned-Poisson demand model", e.to_string()))?;
            // Two fields the scenario states outside `params`, and the outer spelling
            // wins: `duration` is the run's, not the demand model's, and `rate_veh_per_h`
            // is the friendlier unit for the same quantity as `arrival_rate_per_s`.
            params.duration = Duration::from_secs_f64(scenario.time.duration_s);
            if let Some(rate) = d.rate_veh_per_h {
                params.arrival_rate_per_s = rate / 3600.0;
            }
            if !shares.is_empty() {
                params.fleet = v2xw_mobility::demand::FleetMix::from_shares(&shares);
            }
            // The thinning is exact only while the candidate boost covers every multiplier
            // the timeline puts in force (`PoissonParams::candidate_boost`), so the boost is
            // raised to the timeline's peak. A scenario with no demand event has a peak of 1
            // and keeps the draw sequence it always had.
            let peak = crate::timeline::demand_peak(scenario);
            if peak > params.candidate_boost {
                params.candidate_boost = peak;
            }
            Ok(Box::new(PoissonDemand::new(world, params, od)?))
        }
    }
}

/// The GNSS model the scenario names.
///
/// The default is the Gauss–Markov receiver of 04-models.md §2.10 rather than a perfect
/// one: a node whose belief equals the truth makes every plausibility detector trivially
/// correct, which is the single most misleading default this engine could have.
pub fn build_gnss(_scenario: &Scenario) -> Box<dyn GnssModel> {
    Box::new(v2xw_mobility::GaussMarkovGnss::new(
        v2xw_mobility::gnss::GaussMarkovParams::default(),
    ))
}

/// The radio families a scenario may name a model for in `radio.models`, and the ids
/// each accepts. Everything else is refused by the loader.
pub const RADIO_MODEL_FAMILIES: &[(&str, &[&str])] = &[
    (
        "propagation",
        &[
            v2xw_radio::FreeSpace::ID,
            v2xw_radio::TwoRayGround::ID,
            v2xw_radio::LogDistanceShadowing::ID,
            v2xw_radio::Tr37885::ID,
        ],
    ),
    (
        "fading",
        &[v2xw_radio::NoFading::ID, v2xw_radio::NakagamiFading::ID],
    ),
    ("per", &[v2xw_radio::PerModel::ID]),
    ("phy", &[v2xw_radio::OfdmPhy::ID]),
    ("obstacle", &[v2xw_radio::BuildingShadowing::ID]),
];

/// Which propagation law `radio.models.propagation` selects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PropagationChoice {
    /// `propagation/free-space`: Friis.
    FreeSpace,
    /// `propagation/two-ray-ground`: the flat-earth two-ray model.
    TwoRayGround,
    /// `propagation/log-distance-shadowing` with one fixed preset, or `None` for the
    /// per-link choice from the environment and the vehicle-obstruction state.
    LogDistance(Option<v2xw_radio::LogDistancePreset>),
    /// `propagation/tr37885`: the 3GPP LOS/NLOS/NLOSv state model with its own shadowing.
    Tr37885,
}

/// Which small-scale fading `radio.models.fading` selects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FadingChoice {
    /// `fading/none`.
    None,
    /// `fading/nakagami-m` with one preset.
    Nakagami(v2xw_radio::NakagamiPreset),
}

/// The radio models a scenario named in `radio.models`, parsed. A family it did not name
/// is `None` and gets the tier's default.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RadioModels {
    /// The propagation law.
    pub propagation: Option<PropagationChoice>,
    /// The fading model.
    pub fading: Option<FadingChoice>,
    /// The 802.11p packet-error model's implementation-loss preset.
    pub per: Option<v2xw_radio::PerPreset>,
    /// The 802.11p receiver-sensitivity table.
    pub sensitivity: Option<v2xw_radio::SensitivityPreset>,
    /// The Sommer 2011 fitted row the building obstacle model uses.
    pub building_fit: Option<v2xw_radio::SommerFit>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetParam<T> {
    #[serde(default = "none")]
    preset: Option<T>,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PhyParams {
    #[serde(default)]
    sensitivity: Option<v2xw_radio::SensitivityPreset>,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildingParams {
    #[serde(default)]
    fit: Option<v2xw_radio::SommerFit>,
}

fn none<T>() -> Option<T> {
    None
}

fn params_of<T: serde::de::DeserializeOwned>(
    v: &serde_json::Value,
) -> core::result::Result<T, String> {
    let v = if v.is_null() {
        serde_json::json!({})
    } else {
        v.clone()
    };
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// Parses `radio.models`, returning every problem with the dotted path it is at.
///
/// # Errors
/// One `(path, reason)` per unknown family, unknown id, or parameter that does not fit
/// the model.
pub fn radio_models(
    scenario: &Scenario,
) -> core::result::Result<RadioModels, Vec<(String, String)>> {
    let mut out = RadioModels::default();
    let mut errors = Vec::new();
    for (family, choice) in &scenario.radio.models {
        let path = format!("radio.models.{family}");
        let Some((_, ids)) = RADIO_MODEL_FAMILIES.iter().find(|(f, _)| f == family) else {
            errors.push((
                path,
                format!(
                    "'{family}' is not a radio family this build selects a model for; \
                     selectable: {}",
                    RADIO_MODEL_FAMILIES
                        .iter()
                        .map(|(f, _)| *f)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
            continue;
        };
        if !ids.contains(&choice.id.as_str()) {
            errors.push((
                format!("{path}.id"),
                format!(
                    "'{}' is not a {family} model this build ships; choose one of: {}",
                    choice.id,
                    ids.join(", ")
                ),
            ));
            continue;
        }
        let bad = |e: String| {
            (
                format!("{path}.params"),
                format!("do not fit {}: {e}", choice.id),
            )
        };
        match family.as_str() {
            "propagation" => {
                let chosen = match choice.id.as_str() {
                    v2xw_radio::FreeSpace::ID => PropagationChoice::FreeSpace,
                    v2xw_radio::TwoRayGround::ID => PropagationChoice::TwoRayGround,
                    v2xw_radio::Tr37885::ID => PropagationChoice::Tr37885,
                    _ => match params_of::<PresetParam<v2xw_radio::LogDistancePreset>>(
                        &choice.params,
                    ) {
                        Ok(p) => PropagationChoice::LogDistance(p.preset),
                        Err(e) => {
                            errors.push(bad(e));
                            continue;
                        }
                    },
                };
                out.propagation = Some(chosen);
            }
            "fading" => {
                if choice.id == v2xw_radio::NoFading::ID {
                    out.fading = Some(FadingChoice::None);
                } else {
                    match params_of::<PresetParam<v2xw_radio::NakagamiPreset>>(&choice.params) {
                        Ok(p) => {
                            out.fading = Some(FadingChoice::Nakagami(
                                p.preset.unwrap_or(v2xw_radio::NakagamiPreset::FixedMedium),
                            ));
                        }
                        Err(e) => errors.push(bad(e)),
                    }
                }
            }
            "per" => match params_of::<PresetParam<v2xw_radio::PerPreset>>(&choice.params) {
                Ok(p) => out.per = Some(p.preset.unwrap_or(v2xw_radio::PerPreset::Ideal)),
                Err(e) => errors.push(bad(e)),
            },
            "phy" => match params_of::<PhyParams>(&choice.params) {
                Ok(p) => out.sensitivity = p.sensitivity,
                Err(e) => errors.push(bad(e)),
            },
            "obstacle" => match params_of::<BuildingParams>(&choice.params) {
                Ok(p) => {
                    out.building_fit = Some(p.fit.unwrap_or(v2xw_radio::SommerFit::Default));
                }
                Err(e) => errors.push(bad(e)),
            },
            _ => {}
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// The propagation and fading models the scenario names.
///
/// `radio.models.propagation` and `radio.models.fading` choose them when present. The
/// defaults follow the tier (04-models.md §3 tier table): free space with no fading at
/// `abstract`; dual-slope log-distance with correlated shadowing, the preset chosen per
/// link from the environment, and Nakagami `m = 3` fading at `medium` and `high`.
pub fn build_radio(
    scenario: &Scenario,
    world: &World,
) -> (Box<dyn BoxedPropagation>, Box<dyn BoxedFading>) {
    build_radio_at(scenario, world, scenario.radio.tiers.propagation)
}

/// [`build_radio`] at a caller-chosen propagation tier: the focus region's stack.
pub fn build_radio_at(
    scenario: &Scenario,
    world: &World,
    tier: v2xw_core::card::Tier,
) -> (Box<dyn BoxedPropagation>, Box<dyn BoxedFading>) {
    let models = radio_models(scenario).unwrap_or_default();
    let env = world.env_class_at(v2xw_core::geom::Vec3::new(
        (world.bbox.min.x + world.bbox.max.x) * 0.5,
        (world.bbox.min.y + world.bbox.max.y) * 0.5,
        0.0,
    ));
    let default_propagation = match tier {
        v2xw_core::card::Tier::Abstract => PropagationChoice::FreeSpace,
        _ => PropagationChoice::LogDistance(None),
    };
    let propagation: Box<dyn BoxedPropagation> =
        match models.propagation.unwrap_or(default_propagation) {
            PropagationChoice::FreeSpace => Box::new(v2xw_radio::FreeSpace::new(tier)),
            PropagationChoice::TwoRayGround => Box::new(v2xw_radio::TwoRayGround::new(tier)),
            PropagationChoice::Tr37885 => Box::new(v2xw_radio::Tr37885::new(tier, env)),
            PropagationChoice::LogDistance(None) => {
                Box::new(v2xw_radio::LogDistanceShadowing::auto(tier, env))
            }
            PropagationChoice::LogDistance(Some(preset)) => {
                Box::new(v2xw_radio::LogDistanceShadowing::new(tier, preset, env))
            }
        };
    let default_fading = match tier {
        v2xw_core::card::Tier::Abstract => FadingChoice::None,
        _ => FadingChoice::Nakagami(v2xw_radio::NakagamiPreset::FixedMedium),
    };
    let fading: Box<dyn BoxedFading> = match models.fading.unwrap_or(default_fading) {
        FadingChoice::None => Box::new(v2xw_radio::NoFading::new()),
        FadingChoice::Nakagami(preset) => Box::new(v2xw_radio::NakagamiFading::new(preset)),
    };
    (propagation, fading)
}

/// Registers the cards of the radio models a run composes, so the manifest pins them.
///
/// # Errors
/// [`EngineError::Registry`] if a card does not validate.
pub fn register_radio(
    registry: &mut Registry,
    propagation: &dyn BoxedPropagation,
    fading: &dyn BoxedFading,
    phy: &v2xw_radio::OfdmPhy,
) -> Result<()> {
    use v2xw_core::model::Model;
    for card in [
        propagation.card().clone(),
        fading.card().clone(),
        phy.card().clone(),
    ] {
        if !registry.contains(&card.id) {
            registry.register(card)?;
        }
    }
    Ok(())
}

/// Where each node's generator sits on the time axis.
///
/// The default is [`v2xw_msg::GenerationTiming::default`]: an independent uniform phase
/// per node over the 100 ms nominal period of both the BSM (SAE J2945/1) and the CAM's
/// `T_GenCamMin` (EN 302 637-2 §6.1.3), and ns-3's 10 ms hand-off jitter. A scenario
/// overrides either through `messages.generator.params`:
///
/// ```yaml
/// messages:
///   generator:
///     id: generator/timing-phase-jitter
///     params: { phase_window_ms: 0, max_jitter_ms: 0 }   # every node on one grid
/// ```
///
/// `phase_window_ms: 0, max_jitter_ms: 0` is the synchronised behaviour this engine had
/// before the model existed, kept so the contention it causes can still be studied.
pub fn generation_timing(scenario: &Scenario) -> v2xw_msg::GenerationTiming {
    let mut t = v2xw_msg::GenerationTiming::default();
    if let Some(choice) = &scenario.messages.generator {
        let ms = |key: &str| {
            choice
                .params
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0)
                .map(|v| Duration::from_nanos((v * 1e6).round() as u64))
        };
        if let Some(w) = ms("phase_window_ms") {
            t.phase_window = w;
        }
        if let Some(j) = ms("max_jitter_ms") {
            t.max_jitter = j;
        }
    }
    t
}

/// Registers the generation-timing card in force, so the manifest pins the phase window
/// and the jitter a run used.
///
/// # Errors
/// [`EngineError::Registry`] if the card does not validate.
pub fn register_generation_timing(
    registry: &mut Registry,
    timing: v2xw_msg::GenerationTiming,
) -> Result<()> {
    let card = timing.card();
    if !registry.contains(&card.id) {
        registry.register(card)?;
    }
    Ok(())
}

/// What obstructs a radio link: the obstacle stack of 04-models.md §3.5, as far as this
/// build composes it.
///
/// * **Buildings** — `obstacle/building/sommer-2011`: `β` dB per exterior wall the
///   straight path crosses plus `γ` dB per metre of it inside a footprint, from the
///   world's own building footprints, with the 2.5-D refinement that a roof below both
///   antennas does not block. On when `world.buildings.enabled` is true (the default) and
///   the propagation tier is `medium` or `high`, which is how the §3 tier table composes
///   it. Until this stack existed every link was evaluated as line of sight, so a signal
///   crossed a Midtown block of 40-storey towers as though it were open road.
/// * **Terrain** — `obstacle/terrain/knife-edge-p526`: ITU-R P.526 knife-edge diffraction
///   over the world's terrain profile, Deygout for multiple edges. On whenever the world
///   carries a terrain grid (`world.terrain.dem`) at the `medium` or `high` tier. The tier
///   table puts terrain diffraction at `high` only; this build applies it at `medium` too
///   because a scenario that loads a DEM has asked for the ground to obstruct, and a flat
///   world makes the model a no-op anyway.
///
/// The abstract tier composes no obstacle: it is free-space by definition.
#[derive(Debug, Default)]
pub struct ObstacleStack {
    /// Building shadowing, when composed.
    pub buildings: Option<v2xw_radio::BuildingShadowing>,
    /// Terrain diffraction, when composed.
    pub terrain: Option<v2xw_radio::TerrainDiffraction>,
    /// Whether the building model's *loss* is charged, or only its geometry classified.
    ///
    /// `propagation/tr37885` decides its NLOS state from the geometry ("different streets:
    /// geometric") and prices it with its own NLOS law, so under it the buildings are
    /// classified and not charged a second time.
    pub building_loss: bool,
}

impl ObstacleStack {
    /// The line-of-sight answer for one path between two antennas.
    pub fn classify(
        &mut self,
        world: &World,
        a: v2xw_core::geom::Vec3,
        b: v2xw_core::geom::Vec3,
    ) -> v2xw_radio::LosResult {
        let mut parts = Vec::with_capacity(2);
        if let Some(buildings) = self.buildings.as_mut() {
            parts.push(buildings.los_cached(world, a, b));
        }
        if let Some(terrain) = self.terrain.as_ref() {
            parts.push(
                <v2xw_radio::TerrainDiffraction as v2xw_radio::ObstacleModel<
                    crate::ctx::EngineCtx<'_>,
                >>::los(terrain, world, a, b, None),
            );
        }
        match parts.len() {
            0 => v2xw_radio::LosResult::clear(),
            1 => parts.pop().expect("one part"),
            _ => v2xw_radio::merge_los(&parts),
        }
    }

    /// The obstacle loss for a classified path, dB, summed in the stack's fixed order.
    ///
    /// `los_path_db` is the line-of-sight path loss the propagation model charged, which
    /// the building model's street-canyon ceiling is measured against
    /// ([`v2xw_radio::BuildingShadowing::loss_for_path`]).
    #[allow(clippy::too_many_arguments)]
    pub fn loss_db(
        &mut self,
        ctx: &mut crate::ctx::EngineCtx<'_>,
        tx: &v2xw_radio::RadioEndpoint,
        rx: &v2xw_radio::RadioEndpoint,
        los: &v2xw_radio::LosResult,
        f_hz: f64,
        los_path_db: f64,
    ) -> f64 {
        let mut terms = [0.0f64; 2];
        if self.building_loss
            && let Some(buildings) = self.buildings.as_ref()
        {
            terms[0] = buildings.loss_for_path(los, tx.pos.distance(rx.pos), f_hz, los_path_db);
        }
        if let Some(terrain) = self.terrain.as_mut() {
            terms[1] = v2xw_radio::ObstacleModel::obstacle_loss_db(terrain, ctx, tx, rx, los, f_hz);
        }
        v2xw_core::math::sum_ordered(terms)
    }

    /// Registers the cards of the models in the stack, so the manifest pins them.
    ///
    /// # Errors
    /// [`EngineError::Registry`] if a card does not validate.
    pub fn register(&self, registry: &mut Registry) -> Result<()> {
        use v2xw_core::model::Model;
        let mut cards = Vec::new();
        if let Some(b) = &self.buildings {
            cards.push(b.card().clone());
        }
        if let Some(t) = &self.terrain {
            cards.push(t.card().clone());
        }
        for card in cards {
            if !registry.contains(&card.id) {
                registry.register(card)?;
            }
        }
        Ok(())
    }
}

/// The obstacle stack the scenario selects; see [`ObstacleStack`].
///
/// `propagation/tr37885` decides its NLOS state from the same building geometry and
/// prices it with its own NLOS law; charging the Sommer term on top would count every
/// building twice, so under it the buildings are classified and not charged.
/// `radio.models.obstacle` picks the Sommer fitted row.
pub fn build_obstacles(scenario: &Scenario, world: &World) -> ObstacleStack {
    let tier = scenario.radio.tiers.propagation;
    let models = radio_models(scenario).unwrap_or_default();
    if tier == v2xw_core::card::Tier::Abstract {
        return ObstacleStack::default();
    }
    let own_nlos_law = models.propagation == Some(PropagationChoice::Tr37885);
    let fit = models
        .building_fit
        .unwrap_or(v2xw_radio::SommerFit::Default);
    ObstacleStack {
        building_loss: !own_nlos_law,
        buildings: (scenario.world.buildings.enabled && !world.buildings.is_empty())
            .then(|| v2xw_radio::BuildingShadowing::with_fit(tier, fit)),
        terrain: world
            .terrain
            .is_some()
            .then(|| v2xw_radio::TerrainDiffraction::new(tier)),
    }
}

/// A focus region (02-architecture.md §7.3) and the radio stack that runs inside it.
///
/// `v2xw_radio::FocusPlan` holds the coupling rule: a link whose receiver is inside the
/// region is decided by the focus tier's PHY (at `high`, preamble capture and the
/// per-window SINR); a link with both ends inside gets the focus tier's propagation (at
/// `high`, the weather attenuation term) and fading; an inbound link — transmitter
/// outside, receiver inside — gets the surrounding propagation **without** a fading draw
/// (rule 3's "deterministic" crossing); an outbound link is the surrounding tier's, and
/// is the one direction that carries the documented boundary bias.
///
/// Medium access stays one model for the whole world: a contention window cannot be
/// half-modelled, and `FocusPlan::warnings` says so for a `mac: high` focus.
pub struct FocusStack {
    /// The region and the coupling rule.
    pub plan: v2xw_radio::FocusPlan,
    /// The propagation model inside the region.
    pub propagation: Box<dyn BoxedPropagation>,
    /// The fading model inside the region.
    pub fading: Box<dyn BoxedFading>,
}

impl core::fmt::Debug for FocusStack {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FocusStack")
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

/// The focus region `radio.tiers.focus` declares, or `None`.
///
/// A `follow` region is a disc the engine re-centres on the followed node at every
/// mobility step; until that node exists it sits nowhere, so no link is in focus. A `bbox`
/// region is projected into the world's frame once.
pub fn build_focus(scenario: &Scenario, world: &World) -> Option<FocusStack> {
    use v2xw_core::card::Tier;
    let focus = scenario.radio.tiers.focus.as_ref()?;
    let t = &scenario.radio.tiers;
    let outside = v2xw_radio::RadioTierSet {
        propagation: t.propagation,
        phy: t.phy,
        mac: t.mac,
    };
    let inside = v2xw_radio::RadioTierSet {
        propagation: t.propagation.max(focus.tier),
        phy: t.phy.max(focus.tier),
        // One MAC for the world; see the type's documentation.
        mac: t.mac,
    };
    let (shape, follows) = match &focus.region {
        crate::scenario::schema::FocusRegion::Follow { node, radius_m } => (
            v2xw_radio::FocusShape::circle(FOCUS_NOWHERE, *radius_m),
            Some(NodeId::new(*node)),
        ),
        crate::scenario::schema::FocusRegion::Bbox { bbox } => {
            let origin: v2xw_core::geo::GeoOrigin = world.origin.into();
            let a = origin.to_enu(bbox.min_lat_deg, bbox.min_lon_deg, 0.0);
            let b = origin.to_enu(bbox.max_lat_deg, bbox.max_lon_deg, 0.0);
            (
                v2xw_radio::FocusShape::bbox(v2xw_core::geom::Bbox::new(a, b)),
                None,
            )
        }
        // `FocusRegion` is `#[non_exhaustive]`; a region added upstream is refused by the
        // loader until this match learns it.
        #[allow(unreachable_patterns)]
        _ => return None,
    };
    let mut plan = v2xw_radio::FocusPlan::new(shape, inside, outside);
    plan.follows = follows;
    let crossing = if t.propagation == Tier::Abstract {
        Tier::Medium
    } else {
        t.propagation
    };
    let plan = plan.with_crossing_propagation(crossing);
    let (propagation, fading) = build_radio_at(scenario, world, inside.propagation);
    Some(FocusStack {
        plan,
        propagation,
        fading,
    })
}

/// Where a `follow` region sits before its node exists: far outside any world.
pub const FOCUS_NOWHERE: v2xw_core::geom::Vec3 = v2xw_core::geom::Vec3 {
    x: 1.0e12,
    y: 1.0e12,
    z: 0.0,
};

/// The weather the run starts in.
pub fn initial_weather(scenario: &Scenario) -> WeatherState {
    weather_of(
        scenario.weather.initial,
        scenario.weather.intensity,
        scenario.weather.visibility_m,
        scenario.weather.surface,
    )
}

/// A [`WeatherState`] from a scenario's four fields.
///
/// The surface condition is a *separate* field because it does not follow from the kind —
/// a road can still be wet after the rain stops, and black ice happens under a clear sky
/// (03-interfaces.md §3). When the scenario leaves it out, the mapping below is this
/// engine's stated default rather than a physical claim, and it is the conservative one:
/// precipitation wets or freezes the surface, fog and wind do not touch it.
pub fn weather_of(
    kind: WeatherKind,
    intensity: f64,
    visibility_m: Option<f64>,
    surface: Option<SurfaceCondition>,
) -> WeatherState {
    if kind == WeatherKind::Clear && visibility_m.is_none() && surface.is_none() {
        return WeatherState::CLEAR;
    }
    let surface = surface.unwrap_or(match kind {
        WeatherKind::Rain => SurfaceCondition::Wet,
        WeatherKind::Snow => SurfaceCondition::Snow,
        WeatherKind::Sleet => SurfaceCondition::Ice,
        // `WeatherKind` is `#[non_exhaustive]`, so a kind added upstream lands here. Dry
        // is the conservative default: it claims no grip penalty this engine cannot
        // justify, and the scenario can always state the surface itself.
        _ => SurfaceCondition::Dry,
    });
    WeatherState::new(
        kind,
        intensity,
        visibility_m.unwrap_or(f64::INFINITY),
        surface,
    )
}

/// The metric providers the scenario selects.
///
/// `metrics: [all]` means every provider this build ships. A named list selects by metric
/// *name*, so a provider is installed when it defines at least one metric the scenario
/// asked for — which is the useful reading: a scenario asks for `pdr`, not for
/// `metrics/comms/v1`.
///
/// # Errors
/// [`EngineError::Metrics`] if a provider's definitions do not validate.
pub fn build_metrics(
    scenario: &Scenario,
    registry: &mut Registry,
) -> Result<v2xw_metrics::ProviderSet> {
    let mut set = v2xw_metrics::ProviderSet::new();
    if scenario.metrics.is_empty() {
        return Ok(set);
    }
    let all = scenario.metrics.iter().any(|m| m == "all");
    // Every provider a run can feed, in the crate's fixed order. The runtime diagnostics
    // (wall-clock per simulated second, memory high-water mark) are not among them: the
    // engine reads no clock (02-architecture.md §6.1), so it has nothing to hand them, and
    // installing them would put a column of refusals in every run.
    let candidates: Vec<Box<dyn v2xw_metrics::MetricProvider + Send>> = vec![
        Box::new(v2xw_metrics::comms::CommsProvider::new(0)),
        Box::new(v2xw_metrics::latency::LatencyProvider::new()),
        Box::new(v2xw_metrics::awareness::AwarenessProvider::new(0)),
        Box::new(v2xw_metrics::load::LoadProvider::new(0)),
        Box::new(v2xw_metrics::overhead::OverheadProvider::new(0)),
        Box::new(v2xw_metrics::security::SecurityProvider::new(0)),
        Box::new(v2xw_metrics::detection::DetectionProvider::new()),
        Box::new(v2xw_metrics::safety::SafetyProvider::new(0)),
    ];
    for provider in candidates {
        let wanted = all
            || provider
                .defs()
                .iter()
                .any(|d| scenario.metrics.contains(&d.name.to_string()));
        if wanted {
            set.register(registry, provider)?;
        }
    }
    Ok(set)
}

/// The two run-wide facts a node needs that are not in the scenario's `nodes` section.
///
/// Both were left at `NodeConfig`'s defaults by the Phase 1 build, and both are wrong by
/// default in a way that is invisible in a record:
///
/// * `origin` is the world's geodetic anchor. Both message formats carry latitude and
///   longitude, so a node that does not know where world `(0, 0, 0)` is encodes every
///   position about **null island** — a perfectly valid CAM off the coast of Ghana.
/// * `wall` is what `time.t0` means. It is what a 1609.2 `generationTime` and a J2735
///   `secMark` are stamped from, so a node left on the default clock encodes timestamps
///   that have nothing to do with the run's declared civil time.
///
/// Neither is a wall-clock *read*: `wall` comes from the scenario's `time.t0` and
/// `origin` from the world's provenance (02-architecture.md §6.1).
#[derive(Debug, Clone, Copy)]
pub struct NodeEnv {
    /// The geodetic anchor of the world's ENU frame.
    pub origin: GeoOrigin,
    /// The civil instant `SimTime` zero maps to, from `time.t0`.
    pub wall: WallClock,
}

impl NodeEnv {
    /// The environment a run over `world` starting at `wall` gives its nodes.
    pub fn new(world: &World, wall: WallClock) -> Self {
        NodeEnv {
            origin: world.origin.into(),
            wall,
        }
    }
}

/// The hardware profile id the scenario gives a vehicle of this class.
///
/// `nodes.per_class` overrides `nodes.default_obu` by vehicle-class name. The key is the
/// class's own [`VehicleClass::as_str`] spelling, which is the same spelling
/// `actors.vehicles.classes` is keyed by and the one `validate` checks `per_class`
/// against — so a scenario cannot name a class in one section and a different string for
/// the same class in the other.
pub fn obu_profile_id(scenario: &Scenario, class: VehicleClass) -> &str {
    scenario
        .nodes
        .per_class
        .get(class.as_str())
        .map_or(scenario.nodes.default_obu.as_str(), String::as_str)
}

/// Which message services the scenario's `messages.sets` turns on.
///
/// The Phase 1 build left this at [`ServiceSet::BOTH`], so every node generated a CAM
/// *and* a BSM whatever the scenario said — which is why the vertical-slice audit found
/// two different formats on the air in a scenario whose `messages.sets` named one of
/// them. `validate` refuses a set this build has no generator for, so anything that
/// reaches here is one of the two or is deliberately absent.
pub fn service_set(scenario: &Scenario) -> ServiceSet {
    ServiceSet {
        cam: scenario.messages.sets.iter().any(|s| s == "cam"),
        bsm: scenario.messages.sets.iter().any(|s| s == "bsm"),
    }
}

/// The envelope profile `security.envelope` names.
///
/// `validate` restricts the field to the two this build implements, so the fallback is
/// unreachable from a validated scenario and is 1609.2 rather than a panic.
pub fn envelope_profile(scenario: &Scenario) -> EnvelopeProfile {
    match scenario.security.envelope.as_str() {
        "etsi103097" => EnvelopeProfile::EtsiTs103097,
        _ => EnvelopeProfile::Ieee1609Dot2,
    }
}

/// The crypto backend `security.crypto_mode` names.
///
/// Phase 1 acceptance criterion 3 requires a `real` run and a `modeled` run to produce
/// identical event logs apart from the manifest, and `NodeConfig::crypto_mode` is the
/// only thing either mode changes. Until now the scenario's choice reached the *manifest*
/// and not the nodes, so a scenario asking for `real` got modelled cryptography and a
/// manifest that said otherwise.
pub fn crypto_mode(scenario: &Scenario) -> CryptoMode {
    match scenario.security.crypto_mode {
        crate::scenario::schema::CryptoModeSpec::Real => CryptoMode::Real,
        crate::scenario::schema::CryptoModeSpec::Modeled => CryptoMode::Modeled,
    }
}

/// The certificate-attachment cadence `security.signer_id_policy` names.
///
/// 05-protocols.md §2.4 expresses both readings as an interval, and the scenario gives it
/// in milliseconds. `full_cert_every_ms: 0` with `digest_otherwise: false` means "a
/// certificate on every message", which is TS 103 097 §7.1.2's DENM rule; `validate`
/// refuses the contradictory combination of zero with `digest_otherwise` true.
pub fn signer_id_policy(scenario: &Scenario) -> SignerIdPolicy {
    let p = &scenario.security.signer_id_policy;
    if p.full_cert_every_ms == 0 {
        return SignerIdPolicy::ALWAYS_CERTIFICATE;
    }
    if !p.digest_otherwise {
        return SignerIdPolicy::ALWAYS_CERTIFICATE;
    }
    SignerIdPolicy {
        full_cert_every: Some(Duration::from_millis(p.full_cert_every_ms)),
        always_certificate: false,
    }
}

/// The CAM `stationType` a vehicle class is.
///
/// SUMO's vClass table and the CDD's `TrafficParticipantType` are two vocabularies for
/// the same thing; this is the mapping between them, and it is here rather than in
/// `v2xw-mobility` because it is the *scenario's* composition of a traffic model with a
/// message format and neither crate should know about the other.
pub fn station_type(class: VehicleClass) -> ParticipantType {
    match class {
        VehicleClass::Passenger => ParticipantType::PassengerCar,
        // An ambulance or a fire appliance is a special vehicle in the CDD, which is the
        // category the emergency light bar belongs to rather than a size class.
        VehicleClass::Emergency => ParticipantType::SpecialVehicle,
        VehicleClass::Delivery => ParticipantType::LightTruck,
        VehicleClass::Truck => ParticipantType::HeavyTruck,
        VehicleClass::Trailer => ParticipantType::Trailer,
        VehicleClass::Bus | VehicleClass::Coach => ParticipantType::Bus,
        VehicleClass::Motorcycle => ParticipantType::Motorcycle,
        VehicleClass::Moped => ParticipantType::Moped,
        VehicleClass::Bicycle => ParticipantType::Cyclist,
        VehicleClass::Pedestrian => ParticipantType::Pedestrian,
        VehicleClass::Scooter => ParticipantType::LightVruVehicle,
    }
}

/// Builds one node on the scenario's profile, with a bootstrap credential.
///
/// `class` selects the hardware profile through `nodes.per_class` and the CAM
/// `stationType`; `dims` are the actor's own body dimensions, which both message formats
/// carry. `env` brings the world's geodetic anchor and the scenario's civil clock.
pub fn build_node(
    scenario: &Scenario,
    env: NodeEnv,
    node: NodeId,
    at: SimTime,
    class: VehicleClass,
    dims: Dims,
) -> ObuRuntime {
    let wanted = obu_profile_id(scenario, class);
    let profile = v2xw_node::profiles::get(wanted)
        .cloned()
        .unwrap_or_else(|| {
            v2xw_node::profiles::get(v2xw_node::profiles::REFERENCE_OBU)
                .expect("the reference profile ships with v2xw-node")
                .clone()
        });
    let policy: Box<dyn VerificationPolicy> = match scenario.security.verification_policy.as_str() {
        "verify-all" => Box::new(VerifyAll::new()),
        // The two parameterised policies take one number each, and neither has a published
        // default: `on-demand`'s relevance threshold and `prioritized`'s range are study
        // choices. The values here are the ones `v2xw-node::register_all` registers, so a
        // run and the manifest's card agree.
        "on-demand" => Box::new(OnDemand::new(0.5)),
        _ => Box::new(Prioritized::new(300.0)),
    };
    let (bsm_params, cam_params) = generator_params(scenario);
    let config = NodeConfig {
        tx_power_dbm: TX_POWER_DBM,
        services: service_set(scenario),
        crypto_mode: crypto_mode(scenario),
        wall: env.wall,
        origin: env.origin,
        dims,
        station_type: station_type(class),
        bsm_params,
        cam_params,
        ..NodeConfig::default()
    };
    let mut runtime = ObuRuntime::new(node, profile, policy, config, at);
    apply_compute_tier(&mut runtime, scenario);
    apply_security_profile(&mut runtime, scenario, env);
    bootstrap_credentials(&mut runtime, scenario, node, at);
    runtime
}

/// `nodes.compute_tier`: at `abstract` the node's cryptography costs a microsecond and no
/// node is ever compute-bound; at `medium` and `high` every operation costs its hardware
/// profile's service time and queues behind the node's own servers. The two upper tiers
/// are the same model: nothing in this build adds service-time variability at `high`.
fn apply_compute_tier(runtime: &mut ObuRuntime, scenario: &Scenario) {
    if scenario.nodes.compute_tier == v2xw_core::card::Tier::Abstract {
        runtime.set_compute_unlimited();
    }
}

/// The generation parameters `messages.generator` sets, over the standards' defaults.
///
/// The key names one generator and overrides its card's parameters; the other generator
/// keeps its defaults. Every name and bound was checked by `validate`, so a value that
/// does not read here is one the loader already refused, and the default stands.
pub fn generator_params(
    scenario: &Scenario,
) -> (
    v2xw_msg::generator::BsmGenParams,
    v2xw_msg::generator::CamGenParams,
) {
    use v2xw_msg::generator::{BSM_GENERATOR_ID, BsmGenParams, CAM_GENERATOR_ID, CamGenParams};
    let mut bsm = BsmGenParams::j2945_1();
    let mut cam = CamGenParams::en302637_2();
    let Some(choice) = scenario.messages.generator.as_ref() else {
        return (bsm, cam);
    };
    let ms = |name: &str| -> Option<Duration> {
        choice
            .params
            .get(name)
            .and_then(serde_json::Value::as_f64)
            .map(|v| Duration::from_nanos((v * 1e6).round().max(0.0) as u64))
    };
    let num = |name: &str| choice.params.get(name).and_then(serde_json::Value::as_f64);
    match choice.id.as_str() {
        BSM_GENERATOR_ID => {
            if let Some(d) = ms("nominal_itt_ms") {
                bsm.nominal_itt = d;
            }
            if let Some(d) = ms("min_itt_ms") {
                bsm.min_itt = d;
            }
            if let Some(d) = ms("max_itt_ms") {
                bsm.max_itt = d;
            }
        }
        CAM_GENERATOR_ID => {
            if let Some(d) = ms("t_gen_cam_min_ms") {
                cam.t_gen_cam_min = d;
            }
            if let Some(d) = ms("t_gen_cam_max_ms") {
                cam.t_gen_cam_max = d;
            }
            if let Some(d) = ms("t_check_cam_gen_ms") {
                cam.t_check_cam_gen = d;
            }
            if let Some(n) = num("n_gen_cam") {
                cam.n_gen_cam = n.round().clamp(1.0, 255.0) as u8;
            }
            if let Some(deg) = num("heading_threshold_deg") {
                cam.heading_threshold_rad = deg * (core::f64::consts::PI / 180.0);
            }
            if let Some(m) = num("position_threshold_m") {
                cam.position_threshold_m = m;
            }
            if let Some(v) = num("speed_threshold_mps") {
                cam.speed_threshold_mps = v;
            }
            if let Some(d) = ms("low_frequency_interval_ms") {
                cam.low_frequency_interval = d;
            }
        }
        _ => {}
    }
    (bsm, cam)
}

/// Puts `security.envelope` and `security.signer_id_policy` into a fresh node's security
/// stack.
///
/// A whole replacement rather than a mutation: [`v2xw_node::NodeSecurity`]'s two builder
/// methods take `self` by value, and the stack this replaces was constructed by
/// `ObuRuntime::new` moments ago and holds no signers, no issuer and no peer keys. Called
/// anywhere but immediately after construction it would discard credential state, which
/// is why it is private and why both call sites are in this module.
fn apply_security_profile(runtime: &mut ObuRuntime, scenario: &Scenario, env: NodeEnv) {
    let policy = signer_id_policy(scenario);
    let configured = v2xw_node::NodeSecurity::new(
        env.wall,
        crypto_mode(scenario),
        v2xw_node::secure::PSID_SAFETY,
    )
    .with_profile(envelope_profile(scenario), env.wall)
    // One cadence, from the one scenario field. 05-protocols.md §2.4 gives CAM and BSM
    // different published defaults, and a scenario that states a cadence is overriding
    // both: two stacks behind one number would make the field mean different things for
    // different message types with nothing saying so.
    .with_signer_id_policies(policy, policy);
    *runtime.security_mut() = configured;
}

/// The pseudonym-rotation rule the scenario states (`security.pseudonym_change`).
///
/// The strategy's semantics are `v2xw_proto::pseudonym::PseudonymStrategy`'s, the one place
/// the scenario spelling is mapped, expressed as the node store's [`RotationPolicy`]:
///
/// * `time` — change when the active pseudonym is `period_s` old (default 300 s, the
///   J2945/1 `CERTCHG` interval), however far the vehicle drove.
/// * `distance` — change after `distance_m` of travel (default 2 km, the NYC-pilot rule).
/// * `mix-zone` — change only on leaving a mix zone. No world in this build has mix zones,
///   so no scheduled change happens; forced changes (expiry, revocation) still do.
/// * `silent` — no scheduled change; forced changes still happen.
///
/// This used to ignore the numbers: a period of exactly 300 s selected "5 min *and* 2 km"
/// and any other period "5 min *or* 2 km", so `period_s: 10` in phase2-manhattan.yaml still
/// rotated at five minutes, and the store held a single pseudonym to rotate to anyway.
pub fn rotation_policy(scenario: &Scenario) -> RotationPolicy {
    use v2xw_proto::pseudonym::PseudonymStrategy;
    let p = &scenario.security.pseudonym_change;
    let never = RotationPolicy {
        min_age: Duration::MAX,
        min_distance_m: f64::INFINITY,
        require_both: false,
    };
    match PseudonymStrategy::from_scenario(&p.strategy, p.period_s, p.distance_m) {
        Some(PseudonymStrategy::Time { period }) => RotationPolicy {
            min_age: period,
            ..never
        },
        Some(PseudonymStrategy::Distance { distance_cm }) => RotationPolicy {
            min_distance_m: distance_cm as f64 / 100.0,
            ..never
        },
        // `from_scenario` is non-exhaustive and the validator refuses unknown names; a name
        // that still reaches here gets the schema's default rule.
        None => RotationPolicy {
            min_age: v2xw_proto::pseudonym::CERTCHG_INTERVAL,
            ..never
        },
        Some(_) => never,
    }
}

/// How many pseudonym certificates the bootstrap stand-in installs: the SCMS's batch of 20
/// concurrently valid pseudonyms per week (USDOT SCMS Technical Primer, FHWA-JPO-19-775,
/// pp. 7-8; 05-protocols.md §2.4), so a rotation has somewhere to go.
pub const BOOTSTRAP_PSEUDONYMS: u32 = 20;

/// Installs the pseudonym a node starts with. See the module documentation: this stands in
/// for the credential protocol and does not model it.
pub fn bootstrap_credentials(
    runtime: &mut ObuRuntime,
    scenario: &Scenario,
    node: NodeId,
    at: SimTime,
) {
    let policy = rotation_policy(scenario);
    let store = runtime.stores_mut();
    *store = v2xw_node::Stores {
        certs: core::mem::take(&mut store.certs).with_policy(policy),
        ..core::mem::take(store)
    };
    // One week's batch of pseudonyms, all valid for the whole run; the store rotates
    // within it by the scenario's rule. Provisioning them over the air is the credential
    // protocol's job (`install_provisioned` when the SCMS runs).
    for j in 0..BOOTSTRAP_PSEUDONYMS {
        runtime.stores_mut().certs.insert(CredentialHandle {
            digest: pseudo_signer(node, j),
            // The encoded certificate's *size* is what the envelope overhead depends on,
            // and 04-models.md §9.1 derives 117 bytes for an implicit 1609.2 pseudonym
            // certificate. The bytes themselves are not a real certificate and nothing
            // reads them; a scenario in `real` crypto mode needs the protocol.
            cert_coer: vec![0u8; 117],
            // The handle scheme `install_provisioned` uses: one key per (node, j).
            key: v2xw_sec::KeyId(u64::from(node.index()) << 8 | u64::from(j)),
            i_period: 0,
            j_index: j,
            valid_from: at,
            valid_until: SimTime::MAX,
            state: CredState::Active,
        });
    }
}

/// The physical layer the scenario names.
///
/// `OfdmPhy` at the scenario's PHY tier, with the crate's own defaults for everything the
/// scenario does not select: the EN 302 663 static sensitivity table, the −85 dBm CCA
/// busy threshold of EN 302 571 §4.2.10.1, the per-window SINR capture rule, the hardware
/// noise figure and the standards-ideal NIST error model.
///
/// Every one of those is a *model card* default with a citation, which is why none of them
/// is restated here. The Phase 1 build instead carried a hand-written noise floor
/// (`−174 dBm/Hz + 10·log10(10 MHz) + 9 dB = −95 dBm`) and its own `PerModel`, so the
/// engine and the PHY's card could disagree about the receiver; they now cannot, and the
/// noise floor a run uses is the card's −98 dBm (−104 dBm thermal in 10 MHz plus the 6 dB
/// hardware noise figure) rather than the engine's 3GPP 9 dB figure.
///
/// The abstract tier gets the same instance. `phy/abstract/distance-load-table` is the
/// tier's own model and it needs a **calibrated** table — `v2xw_radio::calibrate` produces
/// one from a `high`-tier run — which this build has not produced, so an abstract-tier
/// scenario runs the link-budget PHY over free-space propagation and no fading. That is a
/// missing calibration artefact, not a missing seam.
///
/// `radio.models.per` selects the implementation-loss preset of the NIST error model
/// (`ideal`, or `sjoberg-atheros`'s measured 5 dB), and `radio.models.phy` the sensitivity
/// table (`etsi-static`, `etsi-dynamic`, `cohda-mk5`).
pub fn build_phy(scenario: &Scenario) -> v2xw_radio::OfdmPhy {
    let models = radio_models(scenario).unwrap_or_default();
    let mut phy = v2xw_radio::OfdmPhy::new(scenario.radio.tiers.phy);
    if let Some(preset) = models.per {
        phy = phy.with_per_model(v2xw_radio::PerModel::new(preset));
    }
    if let Some(sensitivity) = models.sensitivity {
        phy = phy.with_sensitivity(sensitivity);
    }
    phy
}

/// The medium-access model the scenario names, or `None` when the tier models no access.
///
/// `mac/80211p/edca-ocb` at the medium and high tiers. The abstract tier has no MAC at
/// all: 02-architecture.md §7 defines it as a reception probability from a calibrated
/// table, and a contention window inside it would be counted twice.
///
/// `SlottedMac` (`mac/80211p/slotted-abstraction`) exists in `v2xw-radio` and is not
/// selected by any tier here, because nothing in the scenario schema distinguishes the two
/// CSMA abstractions; it is one `radio.models` entry away.
pub fn build_mac(scenario: &Scenario) -> Option<v2xw_radio::EdcaOcbMac> {
    match scenario.radio.tiers.mac {
        v2xw_core::card::Tier::Abstract => None,
        _ => Some(v2xw_radio::EdcaOcbMac::new()),
    }
}

/// The congestion-control model the scenario names, or `None` when the tier models none.
///
/// `dcc/sae/j2945-1-rate-power` for `rat: dsrc-80211p`, because J2945/1 is the congestion
/// control that goes with the US DSRC band plan this engine transmits in (channel 172) and
/// with the 20 dBm Class B default the node profile already carries. The ETSI adaptive and
/// reactive algorithms of TS 102 687 ship in `v2xw-radio` and are the right choice for
/// `rat: lte-v2x-pc5` in ITS-G5 spectrum; nothing selects them yet, and a scenario that
/// needs one is a `radio.models` entry rather than a new model.
///
/// `None` at the abstract MAC tier, which measures no channel busy ratio to feed it.
pub fn build_dcc(scenario: &Scenario) -> Option<v2xw_radio::SaeJ2945Dcc> {
    match scenario.radio.tiers.mac {
        v2xw_core::card::Tier::Abstract => None,
        _ => Some(v2xw_radio::SaeJ2945Dcc::new()),
    }
}

/// Builds one roadside unit's node runtime.
///
/// An [`ObuRuntime`] on an RSU hardware profile with **no message services**: 06-node-
/// models.md §3 describes an RSU as "the same queue/server structure as the OBU with a
/// larger profile" plus roles and failure states. A unit that generated CAMs or BSMs would
/// be a vehicle with a mast, so the service set is empty and what it puts on the air is
/// what the engine's Phase 2 path hands it.
///
/// The roles and failure states now ship, as [`v2xw_node::RsuRuntime`]; this function has
/// not been moved onto it. See `crate::phase2`'s "What is not here" for what that move
/// costs and why it is owed rather than missing.
pub fn build_rsu(
    scenario: &Scenario,
    env: NodeEnv,
    spec: &crate::phase2::RsuSpec,
    node: NodeId,
    at: SimTime,
) -> ObuRuntime {
    let profile = v2xw_node::profiles::get(&spec.profile)
        .cloned()
        .unwrap_or_else(|| {
            v2xw_node::profiles::get(v2xw_node::profiles::REFERENCE_OBU)
                .expect("the reference profile ships with v2xw-node")
                .clone()
        });
    let config = NodeConfig {
        tx_power_dbm: TX_POWER_DBM,
        services: v2xw_node::ServiceSet {
            cam: false,
            bsm: false,
        },
        crypto_mode: crypto_mode(scenario),
        wall: env.wall,
        origin: env.origin,
        // A mast is infrastructure, not a vehicle: the CDD has a category for it and a
        // unit that signed as a passenger car would be a unit a plausibility detector is
        // entitled to disbelieve.
        station_type: ParticipantType::Infrastructure,
        ..NodeConfig::default()
    };
    // `verify-all` at a roadside unit, whatever the vehicles run: a unit that forwards
    // misbehaviour reports has to have verified the report it forwards, and the
    // `prioritized` policy would skip a distant sender — which is every sender, at a mast.
    let mut runtime = ObuRuntime::new(node, profile, Box::new(VerifyAll::new()), config, at);
    apply_compute_tier(&mut runtime, scenario);
    apply_security_profile(&mut runtime, scenario, env);
    bootstrap_credentials(&mut runtime, scenario, node, at);
    runtime
}

/// Replaces a node's bootstrap credential pool with the certificates the SCMS provisioned.
///
/// One [`CredentialHandle`] per provisioned certificate, so the store has something to
/// *rotate* to and two pseudonyms of one device appear on the air inside a run — which is
/// what a linkage resolution needs, since the Misbehaviour Authority correlates two
/// reports about two different pseudonyms.
///
/// The digest stays [`pseudo_signer`]'s, and the validity window and the i-period are the
/// protocol's. See [`crate::phase2`], joint 1, for why that split is the honest one while
/// no `CredentialProtocol` plug-in ships.
pub fn install_provisioned(
    runtime: &mut ObuRuntime,
    scenario: &Scenario,
    node: NodeId,
    creds: &[crate::phase2::ProvisionedCred],
) {
    // A fresh store rather than a cleared one: `CertStore` has no `clear`, and it should
    // not — a credential store that can be emptied in place is one a bug can silently
    // empty. The rotation policy is re-applied from the scenario, exactly as
    // `bootstrap_credentials` set it.
    let policy = rotation_policy(scenario);
    *runtime.stores_mut() = v2xw_node::Stores {
        certs: v2xw_node::CertStore::new().with_policy(policy),
        ..core::mem::take(runtime.stores_mut())
    };
    for cred in creds {
        runtime.stores_mut().certs.insert(CredentialHandle {
            digest: pseudo_signer(node, cred.j),
            cert_coer: vec![0u8; 117],
            key: v2xw_sec::KeyId(u64::from(node.index()) << 8 | u64::from(cred.j)),
            i_period: cred.i,
            j_index: cred.j,
            valid_from: cred.valid_from,
            valid_until: cred.valid_until,
            state: CredState::Active,
        });
    }
}
