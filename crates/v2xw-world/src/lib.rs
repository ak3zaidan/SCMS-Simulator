//! `v2xw-world` — the world: one geometry model, its importers, its provenance and the
//! spatial indices over static geometry.
//!
//! This crate owns everything that does not move. Actors, nodes, radios and messages live
//! elsewhere; a `World` is built once, hashed, and then read by every other crate
//! (02-architecture.md §2, ADR 0010).
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | The `World` struct and everything in it | [`model`] | 03-interfaces.md §2, 04-models.md §1.1 |
//! | Writer-side quantisation, the one grid table | [`quant`] | 12-build-decisions.md D9, ADR 0004 |
//! | R-tree, lane grid, reverse adjacency | [`index`] | 03-interfaces.md §2 |
//! | The engine's own binary and JSON format | [`serde_native`] | 04-models.md §1.1 |
//! | The UI's `vwp-world/1` payload | [`serde_vwp`] | docs/protocol/vwp-v1.md §4 |
//! | The content hash (invariant I-W2) | [`hash`] | 03-interfaces.md §2 |
//! | The procedural generators: grid, radial, random | [`procedural`] | 04-models.md §1.2 |
//! | The OpenStreetMap importer | [`osm`] | 04-models.md §1.2, §1.3 |
//! | Drivable path geometry: corners rounded into arcs | [`curve`] | AASHTO Green Book 2018 §9.5, Table 2-2 |
//! | The SUMO `net.xml` and OpenDRIVE importers | [`sumo`] | 04-models.md §1.1, §1.2 |
//! | The DEM importers, and draping geometry onto one | [`dem`] | 04-models.md §1.4 |
//! | Line of sight over terrain | [`los`] | 04-models.md §1.4, §3.5 |
//! | Errors | [`error`] | — |
//!
//! # Getting one
//!
//! ```
//! use v2xw_world::{ImportOptions, WorldSource, WorldSourceSpec};
//! use v2xw_world::procedural::{GridParams, GridSource};
//!
//! let params = GridParams::legacy();
//! let world = v2xw_world::procedural::grid(&params, &ImportOptions::default())?;
//! assert_eq!(world.counts().junctions, 36);
//! assert!(world.project(world.lane(v2xw_core::ids::LaneId::new(0)).start()).is_some());
//!
//! // The same thing through the plug-in seam:
//! let source = GridSource::new();
//! let same = source.build(
//!     &WorldSourceSpec::procedural("world/source/procedural-grid", serde_json::json!({})),
//!     &ImportOptions::default(),
//! )?;
//! assert_eq!(same.content_hash, world.content_hash);
//! # Ok::<(), v2xw_world::WorldError>(())
//! ```
//!
//! # The four rules this crate keeps
//!
//! 1. **Everything is quantised at construction** ([`quant`], D9), so every artefact is
//!    on its grid and every hash is a hash of integers.
//! 2. **No hash-map iteration reaches an output.** There is no `HashMap` or `HashSet` in
//!    the crate; collections are `Vec`s indexed by dense ids or `BTreeMap`s, and every
//!    spatial query sorts its results by id before returning them.
//! 3. **Ids are dense and assigned in a documented order** — [`procedural::grid`]
//!    documents the generator's order, and [`model::RoadNetwork::new`] refuses anything
//!    that is not dense.
//! 4. **No standard-library transcendental.** Every `sin`, `cos`, `atan2` goes through
//!    [`v2xw_core::math`], which is the pure-Rust `libm` port, because the standard
//!    library's delegate to a platform libm whose results differ between platforms
//!    (ADR 0003).
//! 5. **One generator draws random numbers, from one declared stream.**
//!    [`procedural::random`] grows an irregular network, and every draw comes from an
//!    [`RngRegistry`](v2xw_core::RngRegistry) stream keyed by
//!    `(RngDomain::plugin(model id), EntityRef::custom(model id, 0))` and seeded from its
//!    own `seed` parameter. Everything else in the crate — every importer, the grid, the
//!    spider, the DEM resampler — is a pure function of its input.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod curve;
pub mod dem;
pub mod error;
pub mod hash;
pub mod index;
pub mod los;
pub mod model;
pub mod osm;
pub mod procedural;
pub mod quant;
pub mod serde_native;
pub mod serde_vwp;
pub mod sumo;

use serde::{Deserialize, Serialize};
use v2xw_core::card::ModelCard;

pub use dem::{DemAnomaly, DemFormat, DemOptions, DemRaster, DemReport, DemSource, DrapeOptions};
pub use error::{Result, WorldError};
pub use index::{IndexOptions, IndexStats, LaneMatch};
pub use los::{ProfileParams, TerrainEdge, TerrainLos, TerrainProfile};
pub use model::{
    Building, ClassMask, ConflictMatrix, Connection, Crossing, CrossingId, Edge, EnvClass, GeoBbox,
    GeoOrigin, GroupSignal, HeightSource, Interpolation, Junction, JunctionControl, LanduseClass, LanduseZone,
    Lane, LaneKind, LaneProjection, LayerLicence, LodHint, MaterialClass, NetworkCounts,
    Passage, PassageKind, Projection, ROAD_CLEARANCE_M, RoadClass, RoadNetwork, SignalHead, SignalHeadKind, SignalPhase, SignalPlan,
    SignalState, Site, SiteId, SiteKind, SymbolId, SymbolTable, Terrain, Transformation,
    TurnDirection, World, WorldBuilder, WorldParts, WorldProvenance, WorldSourceKind, ZoneId,
    convex_hull_ring, point_in_ring, ring_distance_sq_2d, ring_signed_area_2x, road_meets_building,
    signal_group_wire_id, simplify_rdp,
};
pub use serde_vwp::WorldPayload;
pub use sumo::{SumoAnomaly, SumoImportReport, SumoOptions};

/// Where a world is to be built from (03-interfaces.md §2, 04-models.md §1.2).
///
/// [`WorldSourceSpec::Procedural`] is implemented by [`procedural::GridSource`],
/// [`procedural::radial::RadialSource`] and [`procedural::random::RandomSource`], each
/// answering to its own generator id; [`WorldSourceSpec::OsmXml`] by [`osm::OsmSource`];
/// [`WorldSourceSpec::SumoNet`] by [`sumo::SumoSource`] and [`WorldSourceSpec::OpenDrive`]
/// by [`sumo::OpenDriveSource`]. [`WorldSourceSpec::OsmBbox`] (the Overpass fetch) and
/// [`WorldSourceSpec::LegacyJson`] have none yet, and a [`WorldSource`] that does not
/// implement the specification it is handed returns [`WorldError::UnsupportedSource`]
/// rather than pretending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum WorldSourceSpec {
    /// A procedural generator, by model id, with its parameters as JSON.
    Procedural {
        /// The generator's model id, e.g. `world/source/procedural-grid`.
        generator: String,
        /// Its parameters, in the shape that generator's parameter struct deserialises.
        params: serde_json::Value,
    },
    /// An OpenStreetMap XML extract on disk.
    OsmXml {
        /// Path to the `.osm` or `.osm.xml` file.
        path: String,
        /// The geodetic box to keep, when it is narrower than the file's.
        bbox: Option<GeoBbox>,
    },
    /// An OpenStreetMap extract to be fetched for a bounding box, subject to the Overpass
    /// usage limits of 04-models.md §1.2.
    OsmBbox {
        /// The box to fetch.
        bbox: GeoBbox,
    },
    /// A SUMO `net.xml` network.
    SumoNet {
        /// Path to the network file.
        path: String,
    },
    /// An ASAM OpenDRIVE file, to be imported through `netconvert`.
    OpenDrive {
        /// Path to the `.xodr` file.
        path: String,
    },
    /// The legacy engine's `{"nodes": [[x, y]], "edges": [[a, b, speed?]]}` JSON.
    LegacyJson {
        /// Path to the file.
        path: String,
    },
}

impl WorldSourceSpec {
    /// A procedural specification.
    pub fn procedural(generator: impl Into<String>, params: serde_json::Value) -> Self {
        WorldSourceSpec::Procedural {
            generator: generator.into(),
            params,
        }
    }

    /// A short label for error messages and the provenance record.
    pub fn label(&self) -> String {
        match self {
            WorldSourceSpec::Procedural { generator, .. } => format!("procedural:{generator}"),
            WorldSourceSpec::OsmXml { path, .. } => format!("osm-xml:{path}"),
            WorldSourceSpec::OsmBbox { bbox } => format!(
                "osm-bbox:{},{},{},{}",
                bbox.min_lon_deg, bbox.min_lat_deg, bbox.max_lon_deg, bbox.max_lat_deg
            ),
            WorldSourceSpec::SumoNet { path } => format!("sumo-net:{path}"),
            WorldSourceSpec::OpenDrive { path } => format!("opendrive:{path}"),
            WorldSourceSpec::LegacyJson { path } => format!("json-legacy:{path}"),
        }
    }
}

/// Options every importer and generator honours, and records in the provenance
/// (04-models.md §1.5, invariant I-W3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportOptions {
    /// The import date, ISO-8601 UTC, **supplied by the caller**.
    ///
    /// No part of the engine reads the wall clock (02-architecture.md §6.1), so the
    /// importer cannot date its own work: whoever drives it passes the date in, and it is
    /// written to [`WorldProvenance::imported_at`] and excluded from every digest.
    pub imported_at: String,
    /// Polyline simplification tolerance, metres. `None` keeps every point.
    pub simplify_tolerance_m: Option<f64>,
    /// Junctions closer than this are joined into one (`netconvert --junctions.join`,
    /// whose default of 10 m 04-models.md §1.2 records).
    pub junction_join_m: f64,
    /// Radius within which a `traffic_signals` node is taken to control a junction
    /// (`netconvert --tls.guess-signals`, default 25 m per 04-models.md §1.2).
    pub guess_signals_m: f64,
    /// Metres per storey, for the `building:levels` height rule of 04-models.md §1.3.
    ///
    /// **`TODO: calibrate`.** The OSM wiki states no standard factor; 04-models.md §1.3's
    /// plan is to fit it by regressing Microsoft-estimated heights on OSM
    /// `building:levels` for the Phase 2 city boxes. 3 m is a placeholder, and every
    /// building whose height came from it is marked [`HeightSource::FromLevels`], so the
    /// fit can be applied afterwards to exactly the right buildings.
    pub metres_per_level: f64,
    /// Default building height, metres, when the source gives neither a height nor
    /// levels.
    ///
    /// **`TODO: calibrate`**, per 04-models.md §1.3; buildings that use it are marked
    /// [`HeightSource::Defaulted`].
    pub default_height_m: f64,
    /// Whether to keep building interior holes in the model. The `vwp-world/1` payload
    /// drops them either way (docs/protocol/vwp-v1.md §4.4) and records the drop.
    pub keep_building_holes: bool,
    /// The environment class outside every land-use zone.
    pub default_env: EnvClass,
    /// How the spatial indices are sized.
    pub index_options: IndexOptions,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            imported_at: String::new(),
            simplify_tolerance_m: None,
            junction_join_m: 10.0,
            guess_signals_m: 25.0,
            metres_per_level: 3.0,
            default_height_m: 10.0,
            keep_building_holes: true,
            default_env: EnvClass::Urban,
            index_options: IndexOptions::default(),
        }
    }
}

impl ImportOptions {
    /// The options with an import date.
    #[must_use]
    pub fn imported_at(mut self, when: impl Into<String>) -> Self {
        self.imported_at = when.into();
        self
    }
}

/// A source of worlds: the plug-in seam of 03-interfaces.md §2.
///
/// `build` must be **pure** given `(src, opts)`: same inputs, same world, same content
/// hash, on every platform. That is what makes conformance item W5 (`world.generate` with
/// the same parameters produces the same `world_hash` everywhere) testable.
///
/// 03-interfaces.md §2 writes this as `trait WorldSource: Model`, where `Model` is the
/// base trait that supplies the model card. This crate was written against a `v2xw-core`
/// that had [`ModelCard`] and the registry but not that supertrait, so the card is a
/// method here and this crate depends on nothing that was still moving. The migration,
/// when core's base trait is settled, is two lines: make `Model` the supertrait, and have
/// the implementor hold its card and return a borrow instead of building one per call.
pub trait WorldSource {
    /// The model card (03-interfaces.md §12): what this source is, what it assumes, and
    /// every parameter it reads with its unit, default and source.
    fn card(&self) -> ModelCard;

    /// Builds a world from a source description.
    ///
    /// # Errors
    ///
    /// [`WorldError::UnsupportedSource`] if this source does not implement that
    /// specification, [`WorldError::InvalidParameter`] for parameters it cannot satisfy,
    /// and whatever the geometry itself rejects.
    fn build(&self, src: &WorldSourceSpec, opts: &ImportOptions) -> Result<World>;
}

/// Every model card this crate publishes, in model-id order.
///
/// The registry (03-interfaces.md §12, ADR 0007) must hold a card for every model a run
/// can select, and the manifest pins their versions. Gathering them here means the engine
/// registers the world's models with one call and cannot forget one when a new importer or
/// generator lands: the list is next to the modules it names.
///
/// ```
/// let cards = v2xw_world::model_cards();
/// assert!(cards.iter().any(|c| c.id == "world/source/procedural-radial"));
/// assert!(cards.iter().all(|c| c.validate().is_ok()));
/// ```
pub fn model_cards() -> Vec<ModelCard> {
    let mut cards = vec![
        procedural::card(),
        procedural::radial::card(),
        procedural::random::card(),
        osm::card(),
        los::card(),
    ];
    cards.extend(sumo::model_cards());
    cards.extend(dem::model_cards());
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    cards
}
