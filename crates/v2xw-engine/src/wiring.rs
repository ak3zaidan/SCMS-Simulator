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

use v2xw_core::ids::NodeId;
use v2xw_core::registry::Registry;
use v2xw_core::time::{Duration, SimTime};
use v2xw_core::weather::{SurfaceCondition, WeatherKind, WeatherState};
use v2xw_mobility::{Demand, GnssModel, Mobility, NativeMobility, NoDemand, PoissonDemand};
use v2xw_node::stores::{CredState, CredentialHandle, RotationPolicy, pseudo_signer};
use v2xw_node::{NodeConfig, ObuRuntime, OnDemand, Prioritized, VerificationPolicy, VerifyAll};
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
    let params = v2xw_mobility::EngineParams {
        step: scenario.time.mobility_step(),
        ..v2xw_mobility::EngineParams::default()
    };
    Box::new(NativeMobility::new(params))
}

/// The demand model the scenario names.
///
/// # Errors
/// [`EngineError::Mobility`] if the world admits no trip the demand model could place.
pub fn build_demand(scenario: &Scenario, world: &World) -> Result<Box<dyn Demand>> {
    let d = &scenario.actors.vehicles.demand;
    if d.kind == "mobility/demand/none" {
        return Ok(Box::new(NoDemand::new()));
    }
    let rate_per_s = d.rate_veh_per_h.unwrap_or(0.0) / 3600.0;
    let params = v2xw_mobility::demand::PoissonParams {
        arrival_rate_per_s: rate_per_s,
        duration: Duration::from_secs_f64(scenario.time.duration_s),
        ..v2xw_mobility::demand::PoissonParams::default()
    };
    let od = v2xw_mobility::demand::OdParams::default();
    Ok(Box::new(PoissonDemand::new(world, params, od)?))
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

/// Builds one node on the scenario's profile, with a bootstrap credential.
pub fn build_node(scenario: &Scenario, node: NodeId, at: SimTime) -> ObuRuntime {
    let profile = v2xw_node::profiles::get(&scenario.nodes.default_obu)
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
        ..NodeConfig::default()
    };
    let mut runtime = ObuRuntime::new(node, profile, policy, config, at);
    bootstrap_credentials(&mut runtime, scenario, node, at);
    runtime
}

/// Installs the pseudonym a node starts with. See the module documentation: this stands in
/// for the credential protocol and does not model it.
pub fn bootstrap_credentials(
    runtime: &mut ObuRuntime,
    scenario: &Scenario,
    node: NodeId,
    at: SimTime,
) {
    let period = scenario
        .security
        .pseudonym_change
        .period_s
        .map(Duration::from_secs_f64)
        .unwrap_or(Duration::from_secs(300));
    let policy = if (period.as_secs_f64() - 300.0).abs() < 1e-9 {
        // 05-protocols.md §2.4 admits two readings of the rotation rule; the scenario's
        // period selects which one the store enforces.
        RotationPolicy::J2945_1
    } else {
        RotationPolicy::NYC_PILOT
    };
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
