//! The scenario schema: 03-interfaces.md §13 as Rust types.
//!
//! Every field carries its unit in its name (`duration_s`, `mobility_step_ms`,
//! `tx_power_dbm`) because §13 requires the published schema to state units, and a name is
//! the one place a unit cannot be separated from its value.
//!
//! # Lenient to parse, strict to validate
//!
//! The same rule build decision D11 item 4 settled for model cards applies here, for the
//! same reason: `serde` defaults make a scenario pleasant to write — a file with a seed, a
//! world and a duration is a valid scenario — and [`crate::scenario::validate`] is what
//! enforces the contract. Parsing leniency without validation strictness would defeat it,
//! so no field is defaulted *and* unchecked: everything with a default is either
//! range-checked or cross-checked in `validate`.
//!
//! # Why the world source is `v2xw_world`'s own type
//!
//! `world.source` deserialises straight into [`WorldSourceSpec`], which is the type the
//! importer already takes. A second, scenario-shaped copy of it would be a schema that can
//! express a source the importer cannot build, and the mismatch would surface as a runtime
//! error in the middle of a run rather than as a validation error at load.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::Tier;
use v2xw_core::time::Duration;
use v2xw_core::weather::WeatherKind;
use v2xw_world::{GeoBbox, WorldSourceSpec};

/// The schema version this build writes, and the only one it loads without migrating.
pub const CURRENT_SCHEMA: &str = "v2xw/scenario/1";

/// A complete scenario (03-interfaces.md §13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Schema version, e.g. `v2xw/scenario/1`. Migration is by this string and nothing
    /// else (see [`crate::scenario::migrate`]).
    pub schema: String,
    /// Who wrote it and what it is for.
    #[serde(default)]
    pub meta: Meta,
    /// The master seed. Every sub-stream is derived from it (ADR 0004 §3); a scenario
    /// never sets a sub-stream seed, which is why there is no field for one.
    ///
    /// Accepts `0xdeadbeef`, `"0xdeadbeef"` or a decimal integer.
    #[serde(default, with = "seed_repr")]
    pub seed: u64,
    /// The clock.
    #[serde(default)]
    pub time: Time,
    /// Where the world comes from.
    pub world: WorldSpec,
    /// What moves in it.
    #[serde(default)]
    pub actors: Actors,
    /// Weather at `t0`; fronts arrive through the [`Scenario::events`] timeline.
    #[serde(default)]
    pub weather: Weather,
    /// The radio stack.
    #[serde(default)]
    pub radio: Radio,
    /// The network stack.
    #[serde(default)]
    pub net: Net,
    /// Which messages are generated and how they are encoded.
    #[serde(default)]
    pub messages: Messages,
    /// The security envelope, protocol and policies.
    #[serde(default)]
    pub security: Security,
    /// Which hardware profiles the nodes run on.
    #[serde(default)]
    pub nodes: Nodes,
    /// Attackers, jammers, compromised infrastructure.
    #[serde(default)]
    pub threats: Threats,
    /// Detectors and the misbehaviour-authority pipeline.
    #[serde(default)]
    pub detection: Detection,
    /// Which metrics to compute: a list of ids, or the single entry `all`.
    #[serde(default)]
    pub metrics: Vec<String>,
    /// Which exporters to run.
    #[serde(default)]
    pub exporters: Vec<ExporterSpec>,
    /// The timeline: things that happen at a stated instant.
    #[serde(default)]
    pub events: Vec<TimelineItem>,
    /// A parameter sweep, when this file describes one (08-measurement-and-data.md §4).
    #[serde(default)]
    pub experiment: Option<Experiment>,
}

/// `seed: 0x…`, `seed: "0x…"` or `seed: 12345`.
mod seed_repr {
    use serde::de::{Deserializer, Error as _, Unexpected};
    use serde::{Deserialize, Serialize, Serializer};

    /// Serialises as hexadecimal, which is how §13 writes it and how a run report reads
    /// best beside a derived stream key.
    pub fn serialize<S: Serializer>(seed: &u64, s: S) -> Result<S::Ok, S::Error> {
        format!("0x{seed:016x}").serialize(s)
    }

    /// Accepts a string with or without `0x`, or a plain integer.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Int(u64),
            Text(String),
        }
        match Repr::deserialize(d)? {
            Repr::Int(v) => Ok(v),
            Repr::Text(t) => {
                let trimmed = t.trim();
                let (body, radix) = match trimmed.strip_prefix("0x").or(trimmed.strip_prefix("0X"))
                {
                    Some(hex) => (hex, 16),
                    None => (trimmed, 10),
                };
                u64::from_str_radix(&body.replace('_', ""), radix).map_err(|_| {
                    D::Error::invalid_value(Unexpected::Str(&t), &"a u64, decimal or 0x-hex")
                })
            }
        }
    }
}

/// Who wrote the scenario and what it derives from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    /// A short name.
    #[serde(default)]
    pub name: String,
    /// What it is for.
    #[serde(default)]
    pub description: String,
    /// Authors.
    #[serde(default)]
    pub authors: Vec<String>,
    /// Free-form tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// When it was written, ISO-8601. **Author-supplied**: nothing in the engine reads a
    /// wall clock (02-architecture.md §6.1), so the loader cannot fill this in.
    #[serde(default)]
    pub created: String,
    /// A preset id or a path this scenario overlays (see [`crate::scenario::merge`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// The run's clock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Time {
    /// The civil instant `SimTime` zero corresponds to, RFC 3339. It is what the 1609.2
    /// `generationTime` is stamped from, and it is *scenario data*, not a wall-clock read.
    #[serde(default = "Time::default_t0")]
    pub t0: String,
    /// How long the run lasts, simulated seconds.
    #[serde(default = "Time::default_duration_s")]
    pub duration_s: f64,
    /// The mobility period, milliseconds. ADR 0004 decision 2 allows 10–100.
    #[serde(default = "Time::default_mobility_step_ms")]
    pub mobility_step_ms: u64,
    /// The resolution models are promised, e.g. `1us`. The kernel is always nanoseconds;
    /// this is the guarantee a model may rely on (02-architecture.md §5.1).
    #[serde(default = "Time::default_des_resolution")]
    pub des_resolution: String,
    /// Windows in which radio events are skipped so a long backend experiment can run in
    /// minutes (02-architecture.md §5.4). The manifest records them, and metrics that
    /// depend on radio events are marked `not-observed` inside them.
    #[serde(default)]
    pub time_dilation: Vec<DilationWindow>,
}

impl Time {
    fn default_t0() -> String {
        "2027-03-04T07:00:00Z".to_string()
    }
    fn default_duration_s() -> f64 {
        60.0
    }
    fn default_mobility_step_ms() -> u64 {
        100
    }
    fn default_des_resolution() -> String {
        "1us".to_string()
    }

    /// The mobility period as a [`Duration`].
    pub fn mobility_step(&self) -> Duration {
        Duration::from_millis(self.mobility_step_ms)
    }

    /// The run horizon in nanoseconds, rounded to the nanosecond grid.
    ///
    /// `duration_s` is a float in the file because authors write `0.5`; the kernel is
    /// integer nanoseconds, so the conversion happens once, here, and every consumer takes
    /// the integer.
    pub fn horizon_ns(&self) -> u64 {
        (self.duration_s * 1e9).round().max(0.0) as u64
    }
}

impl Default for Time {
    fn default() -> Self {
        Time {
            t0: Time::default_t0(),
            duration_s: Time::default_duration_s(),
            mobility_step_ms: Time::default_mobility_step_ms(),
            des_resolution: Time::default_des_resolution(),
            time_dilation: Vec::new(),
        }
    }
}

/// A period in which radio events are not generated.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DilationWindow {
    /// When it starts, simulated seconds.
    pub from_s: f64,
    /// When it ends, simulated seconds.
    pub to_s: f64,
}

/// Where the world comes from and what is kept of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldSpec {
    /// The source description, in the importer's own vocabulary.
    pub source: WorldSourceSpec,
    /// The import date, ISO-8601 UTC, **supplied here rather than read from a clock**
    /// (02-architecture.md §6.1). It reaches
    /// [`v2xw_world::ImportOptions::imported_at`] unchanged and is excluded from the
    /// world's content hash.
    #[serde(default)]
    pub imported_at: String,
    /// Building import options.
    #[serde(default)]
    pub buildings: BuildingOptions,
    /// Terrain options.
    #[serde(default)]
    pub terrain: TerrainOptions,
    /// Where an imported world is cached, so a repeated run skips the import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
    /// Which named set of `highway=*` class defaults the OSM importer falls back on.
    ///
    /// **There is no default, and an OSM import is refused without it.** A road class's
    /// fallback speed limit is a statement about a jurisdiction, not a physical constant:
    /// the `sumo-german` table gives 404 Midtown driving lanes a limit above 90 km/h,
    /// 393 of them at 100 km/h on named side streets. The scenario must say which
    /// jurisdiction it means, so this is stated here and nowhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highway_preset: Option<v2xw_world::osm::HighwayPreset>,
}

/// How buildings are imported (04-models.md §1.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildingOptions {
    /// Import building footprints at all.
    #[serde(default = "crate::scenario::schema::truth")]
    pub enabled: bool,
    /// Keep interior holes in footprints.
    #[serde(default = "crate::scenario::schema::truth")]
    pub keep_holes: bool,
    /// Metres per storey for the `building:levels` rule. **`TODO: calibrate`** upstream
    /// ([`v2xw_world::ImportOptions::metres_per_level`]); a scenario that has a local fit
    /// sets it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metres_per_level: Option<f64>,
}

impl Default for BuildingOptions {
    fn default() -> Self {
        BuildingOptions {
            enabled: true,
            keep_holes: true,
            metres_per_level: None,
        }
    }
}

/// How terrain is imported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainOptions {
    /// Path to a DEM raster, when the scenario has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dem: Option<String>,
}

/// Everything that moves or transmits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actors {
    /// Vehicle demand and fleet mix.
    #[serde(default)]
    pub vehicles: Vehicles,
    /// Vulnerable road users.
    #[serde(default)]
    pub vru: Vru,
    /// Roadside units.
    #[serde(default)]
    pub rsus: Vec<Rsu>,
    /// Backend entities and the links between them.
    #[serde(default)]
    pub backend: Backend,
}

/// Vehicle demand and the fleet mix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vehicles {
    /// The demand model and its parameters.
    #[serde(default)]
    pub demand: DemandSpec,
    /// The classes in the fleet, by name, each with its share.
    #[serde(default)]
    pub classes: BTreeMap<String, VehicleClassSpec>,
    /// What fraction of vehicles carry an OBU. `0.0` is a legal scenario: a pure traffic
    /// run with no radio.
    #[serde(default = "Vehicles::default_equipped")]
    pub equipped_fraction: f64,
}

impl Vehicles {
    fn default_equipped() -> f64 {
        1.0
    }
}

impl Default for Vehicles {
    fn default() -> Self {
        Vehicles {
            demand: DemandSpec::default(),
            classes: BTreeMap::new(),
            equipped_fraction: Vehicles::default_equipped(),
        }
    }
}

/// How vehicles arrive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemandSpec {
    /// The demand model's id, e.g. `mobility/demand/poisson`.
    #[serde(default = "DemandSpec::default_kind")]
    pub kind: String,
    /// Vehicles per hour offered to the network, when the model takes a rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_veh_per_h: Option<f64>,
    /// The model's own parameters, in the shape its parameter struct deserialises.
    #[serde(default)]
    pub params: serde_json::Value,
}

impl DemandSpec {
    fn default_kind() -> String {
        "mobility/demand/none".to_string()
    }
}

impl Default for DemandSpec {
    fn default() -> Self {
        DemandSpec {
            kind: DemandSpec::default_kind(),
            rate_veh_per_h: None,
            params: serde_json::Value::Null,
        }
    }
}

/// One class in the fleet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleClassSpec {
    /// Its share of the fleet, `0..=1`. The shares across classes must sum to 1.
    pub fraction: f64,
    /// The hardware profile its OBU runs, or `null` for an unequipped class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obu: Option<String>,
}

/// Vulnerable road users.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vru {
    /// How many pedestrians.
    #[serde(default)]
    pub pedestrians: u32,
    /// How many cyclists.
    #[serde(default)]
    pub cyclists: u32,
    /// What fraction of them carry a device that transmits.
    #[serde(default)]
    pub device_fraction: f64,
}

/// A roadside unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rsu {
    /// The world site it stands at, by site id.
    pub site: u32,
    /// What it does: `crl`, `provisioning-proxy`, `report-forward`, `spat`, `map`, `wsa`.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Its hardware profile id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Its backhaul link model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backhaul: Option<String>,
}

/// Backend entities and their topology.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backend {
    /// The credential-management protocol id, e.g. `proto/scms-camp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// The entities, by role name (`ra`, `pca`, `la1`, `la2`, `ma`, `crlg`, …).
    #[serde(default)]
    pub entities: BTreeMap<String, BackendEntity>,
    /// The links between them.
    #[serde(default)]
    pub links: Vec<BackendLink>,
}

/// One backend entity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendEntity {
    /// Its hardware profile id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Its service model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_model: Option<String>,
    /// Its network model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
}

/// A link between two backend entities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendLink {
    /// The entity the link starts at.
    pub from: String,
    /// The entity it ends at.
    pub to: String,
    /// One-way latency, milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    /// Capacity, megabits per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_mbps: Option<f64>,
}

/// Weather at `t0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weather {
    /// The kind of weather the run starts in.
    #[serde(default)]
    pub initial: WeatherKind,
    /// How hard it is doing it, on `[0, 1]`.
    ///
    /// Dimensionless on purpose, and the same scale [`v2xw_core::WeatherState::intensity`]
    /// uses: a millimetre-per-hour rate is what a radio attenuation model wants and a
    /// fraction of maximum is what a behavioural model wants, and the two do not convert
    /// without a per-kind maximum that no source publishes. Each model's card declares
    /// what its own `1.0` means.
    #[serde(default)]
    pub intensity: f64,
    /// Meteorological visibility, metres. Absent means unrestricted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_m: Option<f64>,
    /// The road surface, which need not follow from the kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<v2xw_core::weather::SurfaceCondition>,
}

impl Default for Weather {
    fn default() -> Self {
        Weather {
            initial: WeatherKind::Clear,
            intensity: 0.0,
            visibility_m: None,
            surface: None,
        }
    }
}

/// The radio stack.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radio {
    /// Which radio access technology.
    #[serde(default)]
    pub rat: Rat,
    /// The fidelity tier of each radio family.
    #[serde(default)]
    pub tiers: RadioTiers,
    /// The model chosen for each family, with its parameters.
    #[serde(default)]
    pub models: BTreeMap<String, ModelChoice>,
}

/// Radio access technology (03-interfaces.md §13).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Rat {
    /// IEEE 802.11p / DSRC, the Phase 1 stack.
    #[default]
    #[serde(rename = "dsrc-80211p")]
    Dsrc80211p,
    /// LTE-V2X PC5 sidelink.
    LteV2xPc5,
    /// NR-V2X PC5 sidelink.
    NrV2xPc5,
    /// More than one of the above at once.
    Hybrid,
}

/// The fidelity tier of each radio family (02-architecture.md §7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioTiers {
    /// Path loss.
    #[serde(default = "RadioTiers::medium")]
    pub propagation: Tier,
    /// The physical layer.
    #[serde(default = "RadioTiers::medium")]
    pub phy: Tier,
    /// Medium access.
    #[serde(default = "RadioTiers::medium")]
    pub mac: Tier,
    /// A region run at a higher tier than the rest of the world (§7.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<Focus>,
}

impl RadioTiers {
    fn medium() -> Tier {
        Tier::Medium
    }
}

impl Default for RadioTiers {
    fn default() -> Self {
        RadioTiers {
            propagation: Tier::Medium,
            phy: Tier::Medium,
            mac: Tier::Medium,
            focus: None,
        }
    }
}

/// A focus region and the tier inside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Focus {
    /// The region: `follow:<node id>` or a geodetic box.
    pub region: FocusRegion,
    /// The tier inside it.
    pub tier: Tier,
}

/// Where the focus region is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FocusRegion {
    /// Centred on a node, moving with it.
    Follow {
        /// Which node.
        node: u32,
        /// The radius around it, metres.
        radius_m: f64,
    },
    /// A fixed geodetic box.
    Bbox {
        /// The box.
        bbox: GeoBbox,
    },
}

/// A model id with its parameter overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelChoice {
    /// The model's registry id.
    pub id: String,
    /// Overrides for its card's defaults. Anything not named here keeps the card's value,
    /// which is what makes the parameter set content-addressable (02-architecture.md §6.5).
    #[serde(default)]
    pub params: serde_json::Value,
}

impl ModelChoice {
    /// A choice with no overrides.
    pub fn new(id: impl Into<String>) -> Self {
        ModelChoice {
            id: id.into(),
            params: serde_json::Value::Null,
        }
    }
}

/// The network stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Net {
    /// Which network layer: `wsmp` or `gn-btp`.
    #[serde(default = "Net::default_layer")]
    pub layer: String,
    /// The fragmenter's model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragmenter: Option<ModelChoice>,
    /// The backhaul link model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backhaul: Option<ModelChoice>,
    /// The cellular uplink model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uu: Option<ModelChoice>,
    /// The backend network model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_net: Option<ModelChoice>,
}

impl Net {
    fn default_layer() -> String {
        "wsmp".to_string()
    }
}

impl Default for Net {
    fn default() -> Self {
        Net {
            layer: Net::default_layer(),
            fragmenter: None,
            backhaul: None,
            uu: None,
            backend_net: None,
        }
    }
}

/// Which messages are generated and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Messages {
    /// Which message sets: `bsm`, `cam`, `denm`, `spat`, `map`, `psm`, `vam`, `cpm`.
    #[serde(default = "Messages::default_sets")]
    pub sets: Vec<String>,
    /// The generator model and its parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<ModelChoice>,
    /// Which codec tier: real `uper` bytes or the validated `size-model` (D2).
    #[serde(default = "Messages::default_codec_tier")]
    pub codec_tier: String,
}

impl Messages {
    fn default_sets() -> Vec<String> {
        vec!["bsm".to_string()]
    }
    fn default_codec_tier() -> String {
        "uper".to_string()
    }
}

impl Default for Messages {
    fn default() -> Self {
        Messages {
            sets: Messages::default_sets(),
            generator: None,
            codec_tier: Messages::default_codec_tier(),
        }
    }
}

/// The security stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Security {
    /// The envelope profile: `ieee1609.2` or `etsi103097`.
    #[serde(default = "Security::default_envelope")]
    pub envelope: String,
    /// The credential-management protocol and its parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ModelChoice>,
    /// The signature primitive, e.g. `ecdsa-p256`.
    #[serde(default = "Security::default_signature")]
    pub signature: String,
    /// `modeled` costs the operations without doing the mathematics; `real` does the
    /// mathematics. Both are recorded in the manifest.
    #[serde(default)]
    pub crypto_mode: CryptoModeSpec,
    /// `verify-all`, `on-demand` or `prioritized`.
    #[serde(default = "Security::default_verification_policy")]
    pub verification_policy: String,
    /// How often a full certificate is attached instead of a digest.
    #[serde(default)]
    pub signer_id_policy: SignerIdPolicySpec,
    /// When a node changes pseudonym.
    #[serde(default)]
    pub pseudonym_change: PseudonymChangeSpec,
}

impl Security {
    fn default_envelope() -> String {
        "ieee1609.2".to_string()
    }
    fn default_signature() -> String {
        "ecdsa-p256".to_string()
    }
    fn default_verification_policy() -> String {
        "verify-all".to_string()
    }
}

impl Default for Security {
    fn default() -> Self {
        Security {
            envelope: Security::default_envelope(),
            protocol: None,
            signature: Security::default_signature(),
            crypto_mode: CryptoModeSpec::default(),
            verification_policy: Security::default_verification_policy(),
            signer_id_policy: SignerIdPolicySpec::default(),
            pseudonym_change: PseudonymChangeSpec::default(),
        }
    }
}

/// Whether the cryptography is done or costed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CryptoModeSpec {
    /// Sign and verify are modelled by their cost and their outcome.
    #[default]
    Modeled,
    /// Real ECDSA over real bytes.
    Real,
}

impl From<CryptoModeSpec> for v2xw_core::manifest::CryptoMode {
    fn from(m: CryptoModeSpec) -> Self {
        match m {
            CryptoModeSpec::Modeled => v2xw_core::manifest::CryptoMode::Modeled,
            CryptoModeSpec::Real => v2xw_core::manifest::CryptoMode::Real,
        }
    }
}

/// How often the full certificate is attached (05-protocols.md §2.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignerIdPolicySpec {
    /// The cadence, milliseconds. 1000 is the J2945/1 reading, 450 the NYC-pilot one;
    /// 05-protocols.md §2.4 admits both, which is why this is a number and not a flag.
    #[serde(default = "SignerIdPolicySpec::default_cadence")]
    pub full_cert_every_ms: u64,
    /// Attach a digest the rest of the time.
    #[serde(default = "crate::scenario::schema::truth")]
    pub digest_otherwise: bool,
}

impl SignerIdPolicySpec {
    fn default_cadence() -> u64 {
        1000
    }
}

impl Default for SignerIdPolicySpec {
    fn default() -> Self {
        SignerIdPolicySpec {
            full_cert_every_ms: SignerIdPolicySpec::default_cadence(),
            digest_otherwise: true,
        }
    }
}

/// When a node changes pseudonym.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PseudonymChangeSpec {
    /// `time`, `distance`, `mix-zone` or `silent`.
    #[serde(default = "PseudonymChangeSpec::default_strategy")]
    pub strategy: String,
    /// The period, seconds, when the strategy is time-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_s: Option<f64>,
    /// The distance, metres, when the strategy is distance-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
}

impl PseudonymChangeSpec {
    fn default_strategy() -> String {
        "time".to_string()
    }
}

impl Default for PseudonymChangeSpec {
    fn default() -> Self {
        PseudonymChangeSpec {
            strategy: PseudonymChangeSpec::default_strategy(),
            period_s: Some(300.0),
            distance_m: None,
        }
    }
}

/// Which hardware the nodes run on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nodes {
    /// The profile every equipped vehicle uses unless its class overrides it.
    #[serde(default = "Nodes::default_obu")]
    pub default_obu: String,
    /// Per-class overrides, by vehicle class name.
    #[serde(default)]
    pub per_class: BTreeMap<String, String>,
    /// The compute tier.
    #[serde(default = "RadioTiers::medium")]
    pub compute_tier: Tier,
    /// The backend tier.
    #[serde(default = "RadioTiers::medium")]
    pub backend_tier: Tier,
}

impl Nodes {
    fn default_obu() -> String {
        "node/obu/reference".to_string()
    }
}

impl Default for Nodes {
    fn default() -> Self {
        Nodes {
            default_obu: Nodes::default_obu(),
            per_class: BTreeMap::new(),
            compute_tier: Tier::Medium,
            backend_tier: Tier::Medium,
        }
    }
}

/// Attackers, jammers and compromised infrastructure.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Threats {
    /// The attackers.
    #[serde(default)]
    pub attackers: Vec<Attacker>,
    /// The jammers.
    #[serde(default)]
    pub jammers: Vec<ModelChoice>,
    /// Roadside units under an attacker's control, by site id.
    #[serde(default)]
    pub compromised_rsus: Vec<u32>,
}

/// One attacker population.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attacker {
    /// The attacker model's id.
    pub id: String,
    /// What share of equipped vehicles are attackers. Mutually exclusive with `count` and
    /// `actor_ids`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    /// How many attackers. Mutually exclusive with `fraction` and `actor_ids`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Exactly which actors are attackers. Mutually exclusive with the other two.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actor_ids: Vec<u32>,
    /// The attacker's parameters.
    #[serde(default)]
    pub params: serde_json::Value,
    /// When it is active, simulated seconds `[from, to)`. Empty means the whole run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<DilationWindow>,
}

/// Detectors and the misbehaviour authority.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detection {
    /// Detectors that run on the node.
    #[serde(default)]
    pub local: Vec<ModelChoice>,
    /// The misbehaviour-authority pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ma: Option<ModelChoice>,
    /// The responder that acts on the authority's decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responder: Option<ModelChoice>,
    /// The perception tier, when the scenario models perception.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perception_tier: Option<Tier>,
}

/// One exporter to run at the end of the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExporterSpec {
    /// The exporter's id, e.g. `ma-dataset-v2`, `receiver-logs`, `recording`.
    pub id: String,
    /// Its options.
    #[serde(default)]
    pub opts: serde_json::Value,
}

/// One item on the scenario timeline.
///
/// §13 writes these as `{t, until?, type, params}`; `type` is a Rust keyword, so the field
/// is `kind` in Rust and `type` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineItem {
    /// When it takes effect, simulated seconds.
    pub t: f64,
    /// When it stops taking effect, simulated seconds. A one-shot item has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<f64>,
    /// What happens.
    #[serde(rename = "type")]
    pub kind: TimelineKind,
    /// Its parameters, in the shape the kind takes.
    #[serde(default, flatten)]
    pub params: BTreeMap<String, serde_json::Value>,
}

/// The kinds of timeline item (03-interfaces.md §13).
///
/// Closed: a scenario cannot invent a control event, because the engine would have nothing
/// to do with it and an unvalidated `type` string is a typo that runs silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum TimelineKind {
    /// Scale the offered demand by `value`.
    #[serde(rename = "demand.multiplier")]
    DemandMultiplier,
    /// A weather front arrives: `value` is a [`WeatherKind`].
    #[serde(rename = "weather.front")]
    WeatherFront,
    /// An attack wave starts: `ids` names the attacker populations.
    #[serde(rename = "attack.wave")]
    AttackWave,
    /// An entity goes down: `target` names it.
    #[serde(rename = "outage")]
    Outage,
    /// A parameter changes: `path` is a dotted path into this scenario, `value` the new
    /// value.
    #[serde(rename = "param.change")]
    ParamChange,
    /// A road closes: `lane` or `edge` names it.
    #[serde(rename = "closure")]
    Closure,
}

impl TimelineKind {
    /// The parameter keys this kind requires, which `validate` checks are present.
    pub const fn required_params(self) -> &'static [&'static str] {
        match self {
            TimelineKind::DemandMultiplier => &["value"],
            TimelineKind::WeatherFront => &["value"],
            TimelineKind::AttackWave => &["ids"],
            TimelineKind::Outage => &["target"],
            TimelineKind::ParamChange => &["path", "value"],
            TimelineKind::Closure => &["target"],
        }
    }

    /// Whether this kind is meaningful with an `until`.
    ///
    /// A weather front does not end — the next front replaces it — and neither does a
    /// parameter change, so an `until` on one is an author error rather than a no-op.
    pub const fn takes_until(self) -> bool {
        matches!(
            self,
            TimelineKind::DemandMultiplier
                | TimelineKind::Outage
                | TimelineKind::AttackWave
                | TimelineKind::Closure
        )
    }
}

/// A parameter sweep (08-measurement-and-data.md §4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Experiment {
    /// Dotted paths into the scenario, each with the values to sweep over.
    #[serde(default)]
    pub sweep: BTreeMap<String, Vec<serde_json::Value>>,
    /// The seeds to repeat each point with.
    #[serde(default)]
    pub seeds: Vec<u64>,
    /// How many replications per (point, seed).
    #[serde(default)]
    pub replications: u32,
}

/// `true`, as a function, for `#[serde(default = …)]`.
pub(crate) fn truth() -> bool {
    true
}
