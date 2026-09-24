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
/// height and lane-pairing fixes changed the Manhattan world's content hash.
pub const IMPORTER_REVISION: u32 = 2;

/// The key a world is cached under: a digest of everything that decides what the import
/// produces — the scenario's `world` section (less `cache` itself), the bytes of the source
/// file when there is one, and the importer's revision ([`IMPORTER_REVISION`]).
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

/// Imports or generates the world the scenario names, with no cache.
fn import_world(scenario: &Scenario) -> Result<World> {
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

/// The propagation and fading models the scenario names.
pub fn build_radio(
    scenario: &Scenario,
    world: &World,
) -> (Box<dyn BoxedPropagation>, Box<dyn BoxedFading>) {
    let tier = scenario.radio.tiers.propagation;
    let env = world.env_class_at(v2xw_core::geom::Vec3::new(
        (world.bbox.min.x + world.bbox.max.x) * 0.5,
        (world.bbox.min.y + world.bbox.max.y) * 0.5,
        0.0,
    ));
    let propagation: Box<dyn BoxedPropagation> = match tier {
        v2xw_core::card::Tier::Abstract => Box::new(v2xw_radio::FreeSpace::new(tier)),
        _ => Box::new(v2xw_radio::LogDistanceShadowing::auto(tier, env)),
    };
    let fading: Box<dyn BoxedFading> = match tier {
        v2xw_core::card::Tier::Abstract => Box::new(v2xw_radio::NoFading::new()),
        _ => Box::new(v2xw_radio::NakagamiFading::new(
            v2xw_radio::NakagamiPreset::FixedMedium,
        )),
    };
    (propagation, fading)
}

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
    let comms = v2xw_metrics::comms::CommsProvider::new(0);
    let wanted = all
        || v2xw_metrics::provider::MetricProvider::defs(&comms)
            .iter()
            .any(|d| scenario.metrics.contains(&d.name.to_string()));
    if wanted {
        set.register(registry, Box::new(comms))?;
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
    let config = NodeConfig {
        tx_power_dbm: TX_POWER_DBM,
        services: service_set(scenario),
        crypto_mode: crypto_mode(scenario),
        wall: env.wall,
        origin: env.origin,
        dims,
        station_type: station_type(class),
        ..NodeConfig::default()
    };
    let mut runtime = ObuRuntime::new(node, profile, policy, config, at);
    apply_security_profile(&mut runtime, scenario, env);
    bootstrap_credentials(&mut runtime, scenario, node, at);
    runtime
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

/// Which reading of the pseudonym-rotation rule the scenario selected.
///
/// 05-protocols.md §2.4 admits two, and the period is what picks between them: 300 s is
/// the J2945/1 rule, anything else the NYC pilot's. It is a function rather than two
/// copies of the same `if`, because the credential store's policy has to be the same one
/// whether the credentials came from the bootstrap stand-in or from the SCMS provisioning.
pub fn rotation_policy(scenario: &Scenario) -> RotationPolicy {
    let period = scenario
        .security
        .pseudonym_change
        .period_s
        .map(Duration::from_secs_f64)
        .unwrap_or(Duration::from_secs(300));
    if (period.as_secs_f64() - 300.0).abs() < 1e-9 {
        RotationPolicy::J2945_1
    } else {
        RotationPolicy::NYC_PILOT
    }
}

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
    // One pseudonym, valid for the whole run. A pool and a rotation schedule are the
    // protocol's job; the store rotates within what it holds.
    runtime.stores_mut().certs.insert(CredentialHandle {
        digest: pseudo_signer(node, 0),
        // The encoded certificate's *size* is what the envelope overhead depends on, and
        // 04-models.md §9.1 derives 117 bytes for an implicit 1609.2 pseudonym
        // certificate. The bytes themselves are not a real certificate and nothing reads
        // them; a scenario in `real` crypto mode needs the protocol.
        cert_coer: vec![0u8; 117],
        key: v2xw_sec::KeyId(u64::from(node.index())),
        i_period: 0,
        j_index: 0,
        valid_from: at,
        valid_until: SimTime::MAX,
        state: CredState::Active,
    });
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
pub fn build_phy(scenario: &Scenario) -> v2xw_radio::OfdmPhy {
    v2xw_radio::OfdmPhy::new(scenario.radio.tiers.phy)
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
