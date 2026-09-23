//! `world/terrain/srtm-30` and `world/terrain/copernicus-glo-30` — the digital elevation
//! model importers (04-models.md §1.4).
//!
//! [`crate::model::Terrain`] and its bilinear sampling already existed; what was missing
//! was a way to fill one from a raster. This module reads two raster formats, resamples
//! whichever it read onto the world's own metre grid, and hands back a [`Terrain`]
//! together with a report of everything it had to guess.
//!
//! | Stage | What happens | Specification |
//! |---|---|---|
//! | 1 | Read the raster: [`read_srtm_hgt`] or [`read_ascii_grid`] | 04-models.md §1.4 |
//! | 2 | Resample onto the world grid, bilinearly | 04-models.md §1.4 ("resampled onto the world grid with bilinear interpolation") |
//! | 3 | Fill voids from the nearest sample, counting every fill | this module |
//! | 4 | Attach, and optionally drape the geometry ([`with_terrain`]) | 04-models.md §1.4 ("lane z is taken from the DEM where the source network has no z") |
//!
//! # The flat case is still the default
//!
//! Nothing here runs unless a caller asks for it. A world with no DEM keeps
//! [`World::ground_height_at`] returning `0.0` at every point, so every scenario that
//! existed before this module produces the same content hash it produced before. Once a
//! terrain grid is attached, the same method answers from the grid — that is the whole of
//! "the ground height comes from the DEM" — and [`DrapeOptions`] additionally lifts the
//! geometry onto it, which *does* change the content hash and is therefore opt-in.
//!
//! # Formats
//!
//! * **SRTM `.hgt`** ([`DemFormat::SrtmHgt`]): the shipping format of NASA SRTMGL1, the
//!   DEM 04-models.md §1.4 names first. A square, header-less block of big-endian
//!   16-bit integers, one degree on a side, `-32768` for a void. The tile's south-west
//!   corner is in its file name (`N40W074.hgt`), which is the only place it is recorded,
//!   so [`parse_srtm_tile`] reads it from there.
//! * **ESRI/USGS ASCII grid** ([`DemFormat::EsriAsciiGrid`]): the textual interchange
//!   format every DEM tool writes, and the path for Copernicus COP-DEM-GLO-30, whose own
//!   distribution is a GeoTIFF this crate deliberately does not parse (a TIFF reader is a
//!   dependency and an attack surface; `gdal_translate -of AAIGrid` is one command).
//!
//! # Determinism
//!
//! Both readers are pure functions of their bytes. The resampler walks the target grid in
//! row-major order and the void filler walks its search rings in a fixed order, so the
//! same raster and the same options give the same grid, byte for byte, on every platform:
//! the arithmetic is multiplication, addition and comparison only, and the geodetic
//! conversion goes through [`crate::model::Projection`], which routes its transcendentals
//! to [`v2xw_core::math`].

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geom::Vec3;

use crate::error::{Result, WorldError};
use crate::model::{
    Building, GeoBbox, GeoOrigin, Interpolation, LayerLicence, Projection, RoadNetwork, Terrain,
    Transformation, World,
};
use crate::quant::{Q_HEIGHT_M, Q_POSITION_M, quantise};

/// The model id of the SRTMGL1 importer (04-models.md §1.4).
pub const SRTM_MODEL_ID: &str = "world/terrain/srtm-30";

/// The model id of the Copernicus COP-DEM-GLO-30 importer (04-models.md §1.4).
pub const COPERNICUS_MODEL_ID: &str = "world/terrain/copernicus-glo-30";

/// The version both importers' cards report.
pub const MODEL_VERSION: &str = "1.0.0";

/// The height a `.hgt` sample carries when the radar returned nothing
/// (a void). SRTM's own sentinel.
pub const SRTM_VOID: i16 = -32_768;

/// The licence identifier recorded for an SRTMGL1 layer.
///
/// 04-models.md §1.4: "openly shared without restriction under EOSDIS data-use guidance;
/// citation requested, not legally required". There is no SPDX identifier for that, so
/// the string names the guidance rather than inventing a licence.
pub const SRTM_LICENCE: &str = "NASA-EOSDIS-open";

/// The citation NASA Earthdata requests for SRTMGL1 (requested, not legally required).
pub const SRTM_ATTRIBUTION: &str = "NASA Shuttle Radar Topography Mission (SRTMGL1), \
                                    NASA EOSDIS Land Processes DAAC";

/// The licence identifier recorded for a Copernicus DEM layer.
pub const COPERNICUS_LICENCE: &str = "Copernicus";

/// The attribution 04-models.md §1.4 requires for **unmodified** Copernicus WorldDEM-30,
/// verbatim.
pub const COPERNICUS_ATTRIBUTION_UNMODIFIED: &str = "© DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH 2014-2018 provided under \
     COPERNICUS by the European Union and ESA; all rights reserved.";

/// The attribution 04-models.md §1.4 requires for **modified** Copernicus WorldDEM-30,
/// verbatim.
///
/// Resampling onto the world grid *is* a modification, so this is the string this
/// importer records.
pub const COPERNICUS_ATTRIBUTION_MODIFIED: &str = "produced using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and © Airbus Defence and \
     Space GmbH 2014-2018 provided under COPERNICUS by the European Union and ESA; all \
     rights reserved.";

/// The liability disclaimer a redistributor of Copernicus data must add
/// (04-models.md §1.4), verbatim.
pub const COPERNICUS_LIABILITY: &str = "The organisations in charge of the Copernicus programme by law or by delegation do \
     not incur any liability for any use of the Copernicus WorldDEM-30";

/// The post spacing of both DEMs 04-models.md §1.4 names, metres.
///
/// SRTMGL1 is 1 arc-second (about 30 m) and COP-DEM-GLO-30 is 30 m, so 30 m is the
/// resolution the data actually carries and the default the resampler targets: a finer
/// target grid interpolates detail that is not in the source, a coarser one throws away
/// detail that is.
pub const DEM_POST_SPACING_M: f64 = 30.0;

// ---------------------------------------------------------------------------
// Anomalies and the report
// ---------------------------------------------------------------------------

/// Something the importer found wrong with the raster, or had to decide for itself.
///
/// Counted in [`DemReport::anomalies`] rather than raised, exactly as the OSM importer
/// counts its own: a DEM tile with a lake in it is not a broken file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DemAnomaly {
    /// A source sample was the format's void sentinel.
    SourceVoid,
    /// A target sample had at least one void among its four source corners, and was
    /// interpolated from the corners that were not void.
    PartialVoidInterpolated,
    /// A target sample had four void corners and was filled from the nearest non-void
    /// sample found by the ring search.
    VoidFilledFromNearest,
    /// A target sample had four void corners and no non-void sample within the search
    /// radius, so it took [`DemOptions::void_default_m`].
    VoidUnfilled,
    /// A target sample fell outside the raster altogether: the requested bounds are
    /// larger than the tile.
    OutsideRaster,
    /// A source sample was outside the plausible range of terrestrial elevations and was
    /// treated as a void.
    ImplausibleElevation,
    /// The ASCII grid's header declared a value this reader ignores.
    UnknownHeaderKey,
    /// The ASCII grid held more values than its header's `ncols × nrows`; the surplus was
    /// dropped.
    SurplusValues,
    /// A value in the ASCII grid's body did not parse as a number and was treated as a
    /// void.
    UnparsableValue,
}

impl DemAnomaly {
    /// Every category, in report order.
    pub const ALL: [DemAnomaly; 9] = [
        DemAnomaly::SourceVoid,
        DemAnomaly::PartialVoidInterpolated,
        DemAnomaly::VoidFilledFromNearest,
        DemAnomaly::VoidUnfilled,
        DemAnomaly::OutsideRaster,
        DemAnomaly::ImplausibleElevation,
        DemAnomaly::UnknownHeaderKey,
        DemAnomaly::SurplusValues,
        DemAnomaly::UnparsableValue,
    ];

    /// A stable kebab-case label for the report.
    pub const fn label(self) -> &'static str {
        match self {
            DemAnomaly::SourceVoid => "source-void",
            DemAnomaly::PartialVoidInterpolated => "partial-void-interpolated",
            DemAnomaly::VoidFilledFromNearest => "void-filled-from-nearest",
            DemAnomaly::VoidUnfilled => "void-unfilled",
            DemAnomaly::OutsideRaster => "outside-raster",
            DemAnomaly::ImplausibleElevation => "implausible-elevation",
            DemAnomaly::UnknownHeaderKey => "unknown-header-key",
            DemAnomaly::SurplusValues => "surplus-values",
            DemAnomaly::UnparsableValue => "unparsable-value",
        }
    }
}

impl core::fmt::Display for DemAnomaly {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

/// What the DEM import read and what it made of it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DemReport {
    /// What was read: a path, or the caller's label for a byte slice.
    pub source_id: String,
    /// SHA-256 of the source bytes, lower-case hex.
    pub source_sha256: String,
    /// Size of the source, bytes.
    pub source_bytes: u64,
    /// Which format was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<DemFormat>,
    /// Which DEM the caller said this is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DemSource>,
    /// The raster's own extent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raster_bbox: Option<GeoBbox>,
    /// Source columns and rows.
    pub raster_size: (u32, u32),
    /// Source samples that were voids.
    pub source_voids: u64,
    /// Target columns and rows.
    pub grid_size: (u32, u32),
    /// Target grid spacing, metres.
    pub cell_m: f64,
    /// The lowest and highest height in the target grid, metres.
    pub height_range_m: (f64, f64),
    /// Target samples interpolated from four non-void corners: the clean case.
    pub samples_clean: u64,
    /// Target samples that needed a void decision of some kind.
    pub samples_repaired: u64,
    /// Every anomaly category that fired, with its count.
    pub anomalies: BTreeMap<DemAnomaly, u64>,
}

impl DemReport {
    /// How many times `kind` fired.
    pub fn anomaly(&self, kind: DemAnomaly) -> u64 {
        self.anomalies.get(&kind).copied().unwrap_or(0)
    }

    /// Every anomaly, of every category, added up.
    pub fn total_anomalies(&self) -> u64 {
        self.anomalies.values().sum()
    }

    /// Records one anomaly.
    fn note(&mut self, kind: DemAnomaly) {
        self.note_n(kind, 1);
    }

    /// Records `n` anomalies of one kind at once.
    ///
    /// A 3 601 x 3 601 tile can hold millions of voids, and counting them one call at a
    /// time is a million map lookups for a number that is already known.
    fn note_n(&mut self, kind: DemAnomaly, n: u64) {
        if n == 0 {
            return;
        }
        *self.anomalies.entry(kind).or_insert(0) += n;
    }

    /// The report as a block of text, for a log or a console.
    pub fn to_text(&self) -> String {
        use core::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "source            {}", self.source_id);
        let _ = writeln!(
            s,
            "sha256            {} ({} bytes)",
            self.source_sha256, self.source_bytes
        );
        let _ = writeln!(
            s,
            "format            {} as {}",
            self.format.map_or("(unknown)", DemFormat::label),
            self.source.map_or("(unstated)", DemSource::label)
        );
        if let Some(b) = self.raster_bbox {
            let _ = writeln!(
                s,
                "raster            {} x {} samples over {:.6},{:.6} .. {:.6},{:.6}",
                self.raster_size.0,
                self.raster_size.1,
                b.min_lon_deg,
                b.min_lat_deg,
                b.max_lon_deg,
                b.max_lat_deg
            );
        } else {
            let _ = writeln!(
                s,
                "raster            {} x {} samples",
                self.raster_size.0, self.raster_size.1
            );
        }
        let _ = writeln!(s, "source voids      {}", self.source_voids);
        let _ = writeln!(
            s,
            "grid              {} x {} at {} m, heights {:.3} .. {:.3} m",
            self.grid_size.0,
            self.grid_size.1,
            self.cell_m,
            self.height_range_m.0,
            self.height_range_m.1
        );
        let _ = writeln!(
            s,
            "samples           {} clean, {} repaired",
            self.samples_clean, self.samples_repaired
        );
        let _ = writeln!(s, "anomalies         {} in total", self.total_anomalies());
        for (kind, count) in &self.anomalies {
            let _ = writeln!(s, "  {:<26} {}", kind.label(), count);
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Formats and sources
// ---------------------------------------------------------------------------

/// Which raster format a file is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DemFormat {
    /// SRTM `.hgt`: big-endian `i16`, square, one degree on a side, no header.
    SrtmHgt,
    /// ESRI/USGS ASCII grid: a six-line header and whitespace-separated values.
    EsriAsciiGrid,
}

impl DemFormat {
    /// A stable label for the report and the provenance.
    pub const fn label(self) -> &'static str {
        match self {
            DemFormat::SrtmHgt => "srtm-hgt",
            DemFormat::EsriAsciiGrid => "esri-ascii-grid",
        }
    }

    /// The format a path's extension names, or `None` for an extension this module does
    /// not read.
    ///
    /// `.hgt` is SRTM; `.asc`, `.arc`, `.aai`, `.grd` and `.txt` are ASCII grids. The
    /// extension is the only hint a header-less format gives, which is why an unknown one
    /// is an error rather than a guess.
    pub fn from_path(path: &Path) -> Option<DemFormat> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "hgt" => Some(DemFormat::SrtmHgt),
            "asc" | "arc" | "aai" | "grd" | "txt" => Some(DemFormat::EsriAsciiGrid),
            _ => None,
        }
    }
}

/// Which published DEM the raster holds — the model id, the licence and the attribution.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DemSource {
    /// NASA SRTMGL1, `world/terrain/srtm-30`.
    Srtm30,
    /// Copernicus COP-DEM-GLO-30, `world/terrain/copernicus-glo-30`.
    CopernicusGlo30,
    /// A raster whose provenance the caller did not state.
    ///
    /// The default, because guessing which DEM a file came from would put a licence claim
    /// in the provenance that nobody made. The layer is then recorded with an empty
    /// licence and a note, which is honest and visibly incomplete.
    #[default]
    Unstated,
}

impl DemSource {
    /// A stable label for the report and the provenance.
    pub const fn label(self) -> &'static str {
        match self {
            DemSource::Srtm30 => "srtm-30",
            DemSource::CopernicusGlo30 => "copernicus-glo-30",
            DemSource::Unstated => "unstated",
        }
    }

    /// The model id of the importer that claims this source, if it has one.
    pub const fn model_id(self) -> Option<&'static str> {
        match self {
            DemSource::Srtm30 => Some(SRTM_MODEL_ID),
            DemSource::CopernicusGlo30 => Some(COPERNICUS_MODEL_ID),
            DemSource::Unstated => None,
        }
    }

    /// The licence identifier to record for this layer.
    pub const fn licence(self) -> &'static str {
        match self {
            DemSource::Srtm30 => SRTM_LICENCE,
            DemSource::CopernicusGlo30 => COPERNICUS_LICENCE,
            DemSource::Unstated => "UNSTATED",
        }
    }

    /// The attribution string a republisher must reproduce, verbatim.
    ///
    /// The Copernicus string is the **modified** wording, because resampling onto the
    /// world grid modifies the data (04-models.md §1.4).
    pub const fn attribution(self) -> Option<&'static str> {
        match self {
            DemSource::Srtm30 => Some(SRTM_ATTRIBUTION),
            DemSource::CopernicusGlo30 => Some(COPERNICUS_ATTRIBUTION_MODIFIED),
            DemSource::Unstated => None,
        }
    }

    /// The layer licence record for this source.
    pub fn layer_licence(self) -> LayerLicence {
        match self.attribution() {
            Some(a) => LayerLicence::with_attribution("terrain", self.licence(), a),
            None => LayerLicence::new("terrain", self.licence()),
        }
    }
}

// ---------------------------------------------------------------------------
// The raster
// ---------------------------------------------------------------------------

/// How a raster's rows and columns are georeferenced.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DemGrid {
    /// A geodetic lattice: an SRTM tile, or an ASCII grid whose header is in degrees.
    ///
    /// Sample `(ix, iy)` sits at `(min_lon_deg + ix·cell_lon_deg,
    /// min_lat_deg + iy·cell_lat_deg)`.
    Geodetic {
        /// Latitude of row 0, degrees north.
        min_lat_deg: f64,
        /// Longitude of column 0, degrees east.
        min_lon_deg: f64,
        /// Row spacing, degrees.
        cell_lat_deg: f64,
        /// Column spacing, degrees.
        cell_lon_deg: f64,
    },
    /// A grid already in the world's own metre frame: an ASCII grid exported in the
    /// network's projected CRS, which is what a SUMO or OpenDRIVE world usually has.
    WorldMetres {
        /// World-local `x` of column 0, metres.
        min_x_m: f64,
        /// World-local `y` of row 0, metres.
        min_y_m: f64,
        /// Column spacing, metres.
        cell_x_m: f64,
        /// Row spacing, metres.
        cell_y_m: f64,
    },
}

/// A raster elevation model as it came off disk: samples plus their georeferencing.
///
/// Row 0 is the **southernmost** row whatever the file's own order was, so every
/// consumer indexes it the same way [`Terrain`] is indexed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DemRaster {
    /// How the samples are georeferenced.
    pub grid: DemGrid,
    /// Number of columns.
    pub ncols: u32,
    /// Number of rows.
    pub nrows: u32,
    /// Heights, row-major, south to north, west to east, `None` for a void.
    pub samples: Vec<Option<f64>>,
    /// Which format it was read from.
    pub format: DemFormat,
}

/// The lowest elevation a terrestrial DEM sample can plausibly hold, metres.
///
/// The Dead Sea shore is about −430 m and the deepest continental ice bed is deeper
/// still, so −500 m is below anything either DEM covers while still rejecting a
/// byte-order mistake, which turns a 100 m hill into −26 000 m. The bound is this
/// module's own choice and is on the model card.
pub const MIN_PLAUSIBLE_ELEVATION_M: f64 = -500.0;

/// The highest elevation a terrestrial DEM sample can plausibly hold, metres.
///
/// Everest is 8 849 m; 9 000 m leaves headroom and still rejects a byte-order mistake.
/// Like [`MIN_PLAUSIBLE_ELEVATION_M`], the bound is this module's own choice and is on the
/// model card with its calibration plan.
pub const MAX_PLAUSIBLE_ELEVATION_M: f64 = 9_000.0;

impl DemRaster {
    /// The sample at `(ix, iy)`, or `None` outside the raster or at a void.
    pub fn sample_at(&self, ix: u32, iy: u32) -> Option<f64> {
        if ix >= self.ncols || iy >= self.nrows {
            return None;
        }
        self.samples
            .get(iy as usize * self.ncols as usize + ix as usize)
            .copied()
            .flatten()
    }

    /// How many samples are voids.
    pub fn void_count(&self) -> u64 {
        self.samples.iter().filter(|s| s.is_none()).count() as u64
    }

    /// The raster's geodetic extent, when it is georeferenced geodetically.
    pub fn geodetic_bbox(&self) -> Option<GeoBbox> {
        match self.grid {
            DemGrid::Geodetic {
                min_lat_deg,
                min_lon_deg,
                cell_lat_deg,
                cell_lon_deg,
            } => Some(GeoBbox::new(
                min_lat_deg,
                min_lon_deg,
                min_lat_deg + cell_lat_deg * f64::from(self.nrows.saturating_sub(1)),
                min_lon_deg + cell_lon_deg * f64::from(self.ncols.saturating_sub(1)),
            )),
            DemGrid::WorldMetres { .. } => None,
        }
    }

    /// The fractional sample indices of a geodetic position, or of a world-metre position
    /// for a projected raster.
    ///
    /// `(fx, fy)` in sample units: `0.0` is the first sample, `ncols - 1` the last.
    fn fractional_index(&self, u: f64, v: f64) -> (f64, f64) {
        match self.grid {
            DemGrid::Geodetic {
                min_lat_deg,
                min_lon_deg,
                cell_lat_deg,
                cell_lon_deg,
            } => (
                (u - min_lon_deg) / cell_lon_deg,
                (v - min_lat_deg) / cell_lat_deg,
            ),
            DemGrid::WorldMetres {
                min_x_m,
                min_y_m,
                cell_x_m,
                cell_y_m,
            } => ((u - min_x_m) / cell_x_m, (v - min_y_m) / cell_y_m),
        }
    }
}

// ---------------------------------------------------------------------------
// Reading: SRTM .hgt
// ---------------------------------------------------------------------------

/// The south-west corner of the SRTM tile a file name names, degrees `(lat, lon)`.
///
/// SRTM tiles are named for the corner they start at: `N40W074.hgt` is the degree square
/// whose south-west corner is 40° N, 74° W. The file itself carries no header at all, so
/// this is the only georeferencing there is; a name that does not match the pattern
/// returns `None` and the caller must state the corner itself.
///
/// The match is case-insensitive and tolerates the `.SRTMGL1.hgt` and `.hgt.zip`-stripped
/// spellings by looking only at the leading seven characters.
pub fn parse_srtm_tile(name: &str) -> Option<(f64, f64)> {
    let bytes: Vec<char> = name.chars().collect();
    if bytes.len() < 7 {
        return None;
    }
    let ns = bytes[0].to_ascii_uppercase();
    let ew = bytes[3].to_ascii_uppercase();
    if !matches!(ns, 'N' | 'S') || !matches!(ew, 'E' | 'W') {
        return None;
    }
    let lat_digits: String = bytes[1..3].iter().collect();
    let lon_digits: String = bytes[4..7].iter().collect();
    let lat: f64 = lat_digits.parse::<u32>().ok()?.into();
    let lon: f64 = lon_digits.parse::<u32>().ok()?.into();
    if lat > 90.0 || lon > 180.0 {
        return None;
    }
    Some((
        if ns == 'S' { -lat } else { lat },
        if ew == 'W' { -lon } else { lon },
    ))
}

/// Reads an SRTM `.hgt` tile.
///
/// `corner` is the tile's south-west corner in degrees `(lat, lon)`, from
/// [`parse_srtm_tile`] or from the caller. The tile is square with `n × n` samples
/// spanning one degree, so the spacing is `1/(n − 1)` degrees: 3 601 samples for SRTMGL1
/// (1 arc-second) and 1 201 for SRTM3 (3 arc-seconds). The file's first row is the
/// **northernmost**; it is flipped here so that row 0 is the southernmost, as
/// [`DemRaster`] and [`Terrain`] both require.
///
/// # Errors
///
/// [`WorldError::Malformed`] if the byte count is not twice a perfect square, or if the
/// side is under two samples.
pub fn read_srtm_hgt(bytes: &[u8], corner: (f64, f64)) -> Result<DemRaster> {
    if bytes.len() % 2 != 0 {
        return Err(WorldError::Malformed {
            offset: bytes.len(),
            problem: format!(
                "an .hgt tile holds 16-bit samples, so its length must be even; got {} bytes",
                bytes.len()
            ),
        });
    }
    let count = bytes.len() / 2;
    let side = isqrt_exact(count).ok_or_else(|| WorldError::Malformed {
        offset: 0,
        problem: format!(
            "an .hgt tile is square, so it holds a perfect square of samples; got {count}"
        ),
    })?;
    if side < 2 {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: format!("an .hgt tile needs at least 2 x 2 samples, got {side} x {side}"),
        });
    }
    let side_u32 = u32::try_from(side).map_err(|_| WorldError::Malformed {
        offset: 0,
        problem: format!("{side} samples a side is more than this reader supports"),
    })?;
    let step = 1.0 / (side as f64 - 1.0);
    let mut samples = vec![None; count];
    for file_row in 0..side {
        // The file runs north to south; the raster runs south to north.
        let iy = side - 1 - file_row;
        for ix in 0..side {
            let at = (file_row * side + ix) * 2;
            let raw = i16::from_be_bytes([bytes[at], bytes[at + 1]]);
            samples[iy * side + ix] = if raw == SRTM_VOID {
                None
            } else {
                let h = f64::from(raw);
                if (MIN_PLAUSIBLE_ELEVATION_M..=MAX_PLAUSIBLE_ELEVATION_M).contains(&h) {
                    Some(h)
                } else {
                    None
                }
            };
        }
    }
    Ok(DemRaster {
        grid: DemGrid::Geodetic {
            min_lat_deg: corner.0,
            min_lon_deg: corner.1,
            cell_lat_deg: step,
            cell_lon_deg: step,
        },
        ncols: side_u32,
        nrows: side_u32,
        samples,
        format: DemFormat::SrtmHgt,
    })
}

/// The integer square root of `n`, or `None` when `n` is not a perfect square.
///
/// Integer arithmetic only: no `sqrt`, so no transcendental and no rounding question.
fn isqrt_exact(n: usize) -> Option<usize> {
    if n == 0 {
        return Some(0);
    }
    let mut low = 1usize;
    let mut high = n.min(1 << 20);
    while low <= high {
        let mid = low + (high - low) / 2;
        let square = mid.checked_mul(mid)?;
        if square == n {
            return Some(mid);
        } else if square < n {
            low = mid + 1;
        } else {
            high = mid - 1;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Reading: ESRI/USGS ASCII grid
// ---------------------------------------------------------------------------

/// Whether an ASCII grid's header coordinates are degrees or metres in the world frame.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AsciiGridCrs {
    /// `xllcorner`/`yllcorner` are degrees of longitude and latitude, `cellsize` is
    /// degrees. What `gdal_translate` writes for an EPSG:4326 DEM, which is how both
    /// DEMs of 04-models.md §1.4 are published.
    #[default]
    Geodetic,
    /// They are metres in the world's own frame: a DEM already reprojected into the
    /// network's CRS, which is the usual case for a SUMO or OpenDRIVE world.
    WorldMetres,
}

impl AsciiGridCrs {
    /// A stable label for the report and the provenance.
    pub const fn label(self) -> &'static str {
        match self {
            AsciiGridCrs::Geodetic => "geodetic",
            AsciiGridCrs::WorldMetres => "world-metres",
        }
    }
}

/// Reads an ESRI/USGS ASCII grid.
///
/// The header is the six documented keys in any order and any case — `ncols`, `nrows`,
/// `xllcorner` or `xllcenter`, `yllcorner` or `yllcenter`, `cellsize`, and the optional
/// `nodata_value` — followed by `nrows × ncols` values, the first row being the
/// **northernmost**. Rows are flipped here so that row 0 is the southernmost.
///
/// # Corner versus centre
///
/// `xllcorner` is the corner of the lower-left **cell**, so that cell's centre — the
/// position its sample stands for — is half a cell further in. `xllcenter` gives the
/// centre directly. The difference is half a cell, 15 m at 30 m posts, and getting it
/// wrong shifts the whole terrain by that much, so both spellings are read and converted
/// to a sample position.
///
/// # Errors
///
/// [`WorldError::Malformed`] if a required header key is missing or unparsable, if the
/// grid is smaller than 2 × 2, or if the body holds fewer values than the header
/// promises. A single unparsable *value* is a void, not an error, and is counted.
pub fn read_ascii_grid(text: &str, crs: AsciiGridCrs, report: &mut DemReport) -> Result<DemRaster> {
    let mut ncols: Option<usize> = None;
    let mut nrows: Option<usize> = None;
    let mut xll: Option<(f64, bool)> = None;
    let mut yll: Option<(f64, bool)> = None;
    let mut cellsize: Option<f64> = None;
    let mut nodata: f64 = -9999.0;
    let mut body_from = 0usize;

    let bad = |problem: String| WorldError::Malformed { offset: 0, problem };

    let mut tokens: Vec<&str> = Vec::new();
    for line in text.lines() {
        tokens.extend(line.split_whitespace());
    }
    // The header is a run of `key value` pairs at the front. It ends at the first token
    // that is not one of the known keys, which is the first data value.
    while body_from + 1 < tokens.len() {
        let key = tokens[body_from].to_ascii_lowercase();
        let value = tokens[body_from + 1];
        let known = match key.as_str() {
            "ncols" => {
                ncols = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| bad(format!("ncols {value:?} is not an integer")))?,
                );
                true
            }
            "nrows" => {
                nrows = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| bad(format!("nrows {value:?} is not an integer")))?,
                );
                true
            }
            "xllcorner" | "xllcenter" => {
                let v = value
                    .parse::<f64>()
                    .map_err(|_| bad(format!("{key} {value:?} is not a number")))?;
                xll = Some((v, key == "xllcenter"));
                true
            }
            "yllcorner" | "yllcenter" => {
                let v = value
                    .parse::<f64>()
                    .map_err(|_| bad(format!("{key} {value:?} is not a number")))?;
                yll = Some((v, key == "yllcenter"));
                true
            }
            "cellsize" | "dx" => {
                cellsize = Some(
                    value
                        .parse::<f64>()
                        .map_err(|_| bad(format!("{key} {value:?} is not a number")))?,
                );
                true
            }
            "nodata_value" | "nodata" => {
                nodata = value
                    .parse::<f64>()
                    .map_err(|_| bad(format!("{key} {value:?} is not a number")))?;
                true
            }
            "dy" | "byteorder" | "nbits" | "pixeltype" | "layout" => {
                // Recognised as a header key so the body does not start here, but not
                // acted on: a BIL header's keys can appear in a hand-made file.
                report.note(DemAnomaly::UnknownHeaderKey);
                true
            }
            _ => false,
        };
        if !known {
            break;
        }
        body_from += 2;
    }

    let ncols = ncols.ok_or_else(|| bad("the header states no ncols".to_string()))?;
    let nrows = nrows.ok_or_else(|| bad("the header states no nrows".to_string()))?;
    let (xll, x_is_centre) = xll.ok_or_else(|| bad("the header states no xll".to_string()))?;
    let (yll, y_is_centre) = yll.ok_or_else(|| bad("the header states no yll".to_string()))?;
    let cellsize = cellsize.ok_or_else(|| bad("the header states no cellsize".to_string()))?;
    if ncols < 2 || nrows < 2 {
        return Err(bad(format!(
            "an ASCII grid needs at least 2 x 2 samples, got {ncols} x {nrows}"
        )));
    }
    if !(cellsize.is_finite() && cellsize > 0.0) {
        return Err(bad(format!("cellsize {cellsize} is not positive")));
    }
    let want = ncols * nrows;
    let body = &tokens[body_from..];
    if body.len() < want {
        return Err(bad(format!(
            "the header promises {want} values ({ncols} x {nrows}) but the body holds {}",
            body.len()
        )));
    }
    if body.len() > want {
        report.note(DemAnomaly::SurplusValues);
    }

    let mut samples = vec![None; want];
    for file_row in 0..nrows {
        let iy = nrows - 1 - file_row;
        for ix in 0..ncols {
            let token = body[file_row * ncols + ix];
            let parsed = token.parse::<f64>();
            samples[iy * ncols + ix] = match parsed {
                Err(_) => {
                    report.note(DemAnomaly::UnparsableValue);
                    None
                }
                Ok(v) if !v.is_finite() || (v - nodata).abs() <= 1e-9 => None,
                Ok(v) if !(MIN_PLAUSIBLE_ELEVATION_M..=MAX_PLAUSIBLE_ELEVATION_M).contains(&v) => {
                    report.note(DemAnomaly::ImplausibleElevation);
                    None
                }
                Ok(v) => Some(v),
            };
        }
    }

    let half = cellsize / 2.0;
    let x0 = if x_is_centre { xll } else { xll + half };
    let y0 = if y_is_centre { yll } else { yll + half };
    let grid = match crs {
        AsciiGridCrs::Geodetic => DemGrid::Geodetic {
            min_lat_deg: y0,
            min_lon_deg: x0,
            cell_lat_deg: cellsize,
            cell_lon_deg: cellsize,
        },
        AsciiGridCrs::WorldMetres => DemGrid::WorldMetres {
            min_x_m: x0,
            min_y_m: y0,
            cell_x_m: cellsize,
            cell_y_m: cellsize,
        },
    };
    Ok(DemRaster {
        grid,
        ncols: u32::try_from(ncols).map_err(|_| bad(format!("{ncols} columns is too many")))?,
        nrows: u32::try_from(nrows).map_err(|_| bad(format!("{nrows} rows is too many")))?,
        samples,
        format: DemFormat::EsriAsciiGrid,
    })
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Everything the resampler reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DemOptions {
    /// The world frame the target grid is expressed in: the origin of the world the
    /// terrain is for.
    pub origin: GeoOrigin,
    /// The target grid's extent in world-local metres, `(min_x, min_y, max_x, max_y)`.
    ///
    /// `None` means "whatever the world spans", which is what
    /// [`import_terrain_for_world`] fills in. A DEM import with neither bounds nor a
    /// world is refused rather than guessed, because the frame decides every coordinate
    /// in the grid.
    pub bounds_m: Option<(f64, f64, f64, f64)>,
    /// How far outside those bounds the grid still reaches, metres.
    ///
    /// One cell by default, so that a link leaving the road network by a few metres still
    /// samples the grid rather than falling off it.
    pub margin_m: f64,
    /// Target grid spacing, metres. Defaults to [`DEM_POST_SPACING_M`].
    pub cell_m: f64,
    /// How the stored grid is interpolated afterwards; recorded in
    /// [`Terrain::interpolation`], as 04-models.md §1.4 requires.
    pub interpolation: Interpolation,
    /// Which published DEM this is, for the licence and attribution record.
    pub source: DemSource,
    /// Whether an ASCII grid's header is in degrees or in world metres.
    pub ascii_grid_crs: AsciiGridCrs,
    /// How far the void filler searches, in source cells.
    pub void_search_cells: u32,
    /// The height a target sample takes when the search found nothing, metres.
    pub void_default_m: f64,
    /// The most target samples the resampler will produce, as a guard against a
    /// bounds-and-spacing combination that would allocate the machine's memory.
    pub max_samples: usize,
}

impl Default for DemOptions {
    fn default() -> Self {
        Self {
            origin: GeoOrigin::NULL_ISLAND,
            bounds_m: None,
            margin_m: DEM_POST_SPACING_M,
            cell_m: DEM_POST_SPACING_M,
            interpolation: Interpolation::Bilinear,
            source: DemSource::Unstated,
            ascii_grid_crs: AsciiGridCrs::Geodetic,
            void_search_cells: 8,
            void_default_m: 0.0,
            max_samples: 4_000_000,
        }
    }
}

impl DemOptions {
    /// The options anchored at a world's origin and covering its extent.
    #[must_use]
    pub fn for_world(mut self, world: &World) -> Self {
        self.origin = world.origin;
        self.bounds_m = Some((
            world.bbox.min.x,
            world.bbox.min.y,
            world.bbox.max.x,
            world.bbox.max.y,
        ));
        self
    }

    /// The options with the source stated.
    #[must_use]
    pub fn source(mut self, source: DemSource) -> Self {
        self.source = source;
        self
    }

    /// The options with a different target spacing.
    #[must_use]
    pub fn cell_m(mut self, cell_m: f64) -> Self {
        self.cell_m = cell_m;
        self
    }

    /// Checks that the options describe a grid that can exist.
    ///
    /// # Errors
    ///
    /// [`WorldError::InvalidParameter`], naming the parameter.
    pub fn validate(&self) -> Result<()> {
        let bad = |parameter: &str, problem: String| WorldError::InvalidParameter {
            parameter: parameter.to_string(),
            problem,
        };
        if !(self.cell_m.is_finite() && self.cell_m > 0.0) {
            return Err(bad(
                "cell_m",
                format!("{} m is not a positive spacing", self.cell_m),
            ));
        }
        if !(self.margin_m.is_finite() && self.margin_m >= 0.0) {
            return Err(bad(
                "margin_m",
                format!("{} m is not a non-negative margin", self.margin_m),
            ));
        }
        if !self.void_default_m.is_finite() {
            return Err(bad(
                "void_default_m",
                format!("{} m is not finite", self.void_default_m),
            ));
        }
        match self.bounds_m {
            None => Err(bad(
                "bounds_m",
                "the target grid needs an extent: pass one, or use \
                 import_terrain_for_world so the world supplies it"
                    .to_string(),
            )),
            Some((min_x, min_y, max_x, max_y)) => {
                for (name, v) in [
                    ("bounds_m.min_x", min_x),
                    ("bounds_m.min_y", min_y),
                    ("bounds_m.max_x", max_x),
                    ("bounds_m.max_y", max_y),
                ] {
                    if !v.is_finite() {
                        return Err(bad(name, format!("{v} is not finite")));
                    }
                }
                if max_x < min_x || max_y < min_y {
                    return Err(bad(
                        "bounds_m",
                        format!("({min_x}, {min_y}) .. ({max_x}, {max_y}) is inverted"),
                    ));
                }
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Resampling
// ---------------------------------------------------------------------------

/// Resamples a raster onto the world's own metre grid (04-models.md §1.4).
///
/// The target grid starts at the requested bounds' south-west corner grown by
/// [`DemOptions::margin_m`], both put on the position quantum so that the grid's origin is
/// reproducible, and steps by [`DemOptions::cell_m`]. Each target sample is bilinear in
/// the source raster; a target sample whose four source corners are not all present is
/// repaired, and every repair is counted.
///
/// # Errors
///
/// [`WorldError::InvalidParameter`] for options that cannot be satisfied or a grid larger
/// than [`DemOptions::max_samples`], and whatever [`Terrain::new`] rejects.
pub fn resample(
    raster: &DemRaster,
    options: &DemOptions,
    report: &mut DemReport,
) -> Result<Terrain> {
    options.validate()?;
    let (min_x, min_y, max_x, max_y) = options
        .bounds_m
        .expect("validate() refuses options with no bounds");
    let origin_x = quantise(min_x - options.margin_m, Q_POSITION_M);
    let origin_y = quantise(min_y - options.margin_m, Q_POSITION_M);
    let span_x = (max_x + options.margin_m) - origin_x;
    let span_y = (max_y + options.margin_m) - origin_y;
    // `+ 2` rather than `+ 1`: the last sample must sit at or beyond the far edge, and a
    // 2 x 2 grid is the smallest `Terrain::new` accepts.
    let nx = ((span_x / options.cell_m).floor() as i64 + 2).clamp(2, i64::from(u32::MAX)) as u32;
    let ny = ((span_y / options.cell_m).floor() as i64 + 2).clamp(2, i64::from(u32::MAX)) as u32;
    let count = nx as usize * ny as usize;
    if count > options.max_samples {
        return Err(WorldError::InvalidParameter {
            parameter: "cell_m".to_string(),
            problem: format!(
                "a {nx} x {ny} grid is {count} samples, over the {} the options allow; \
                 coarsen cell_m or narrow bounds_m",
                options.max_samples
            ),
        });
    }

    let projection = Projection::new(options.origin);
    let mut heights = Vec::with_capacity(count);
    let mut lowest = f64::INFINITY;
    let mut highest = f64::NEG_INFINITY;
    for iy in 0..ny {
        for ix in 0..nx {
            let x = origin_x + f64::from(ix) * options.cell_m;
            let y = origin_y + f64::from(iy) * options.cell_m;
            let (u, v) = match raster.grid {
                DemGrid::Geodetic { .. } => {
                    let (lat, lon) = projection.to_geodetic(x, y);
                    (lon, lat)
                }
                DemGrid::WorldMetres { .. } => (x, y),
            };
            let h = sample_bilinear(raster, u, v, options, report);
            lowest = lowest.min(h);
            highest = highest.max(h);
            heights.push(h);
        }
    }

    report.format = Some(raster.format);
    report.source = Some(options.source);
    report.raster_bbox = raster.geodetic_bbox();
    report.raster_size = (raster.ncols, raster.nrows);
    report.source_voids = raster.void_count();
    report.grid_size = (nx, ny);
    report.cell_m = quantise(options.cell_m, Q_POSITION_M);
    report.height_range_m = if heights.is_empty() {
        (0.0, 0.0)
    } else {
        (quantise(lowest, Q_HEIGHT_M), quantise(highest, Q_HEIGHT_M))
    };

    Terrain::new(
        origin_x,
        origin_y,
        options.cell_m,
        options.cell_m,
        nx,
        ny,
        heights,
        options.interpolation,
    )
}

/// One target sample: bilinear over the four surrounding source samples, repaired when
/// any of them is a void.
///
/// The repair ladder, in order:
///
/// 1. four corners present — the plain bilinear weight;
/// 2. some corners present — the mean of those that are, which is the bilinear value with
///    the missing weights removed;
/// 3. no corners present — the nearest non-void sample within
///    [`DemOptions::void_search_cells`], found by an expanding ring walked in a fixed
///    order;
/// 4. nothing found — [`DemOptions::void_default_m`].
///
/// Every rung but the first is counted in the report, so a terrain built mostly out of
/// repairs cannot look like a measurement.
fn sample_bilinear(
    raster: &DemRaster,
    u: f64,
    v: f64,
    options: &DemOptions,
    report: &mut DemReport,
) -> f64 {
    let (fx, fy) = raster.fractional_index(u, v);
    let last_x = f64::from(raster.ncols.saturating_sub(1));
    let last_y = f64::from(raster.nrows.saturating_sub(1));
    if !(fx.is_finite() && fy.is_finite()) || fx < 0.0 || fy < 0.0 || fx > last_x || fy > last_y {
        report.note(DemAnomaly::OutsideRaster);
        report.samples_repaired += 1;
        return nearest_non_void(raster, fx, fy, options, report).unwrap_or(options.void_default_m);
    }
    let ix = (fx.floor() as u32).min(raster.ncols.saturating_sub(2));
    let iy = (fy.floor() as u32).min(raster.nrows.saturating_sub(2));
    let tx = fx - f64::from(ix);
    let ty = fy - f64::from(iy);
    let corners = [
        (raster.sample_at(ix, iy), (1.0 - tx) * (1.0 - ty)),
        (raster.sample_at(ix + 1, iy), tx * (1.0 - ty)),
        (raster.sample_at(ix, iy + 1), (1.0 - tx) * ty),
        (raster.sample_at(ix + 1, iy + 1), tx * ty),
    ];
    let present = corners.iter().filter(|(h, _)| h.is_some()).count();
    if present == 4 {
        report.samples_clean += 1;
        // Written as the sum of four weighted corners in a fixed order, which is what
        // makes it reproducible; the weights sum to 1 exactly.
        let mut acc = 0.0;
        for (h, w) in corners {
            acc += h.unwrap_or(0.0) * w;
        }
        return acc;
    }
    report.samples_repaired += 1;
    if present > 0 {
        report.note(DemAnomaly::PartialVoidInterpolated);
        let mut weight = 0.0;
        let mut acc = 0.0;
        for (h, w) in corners {
            if let Some(h) = h {
                acc += h * w;
                weight += w;
            }
        }
        if weight > 0.0 {
            return acc / weight;
        }
        // Every present corner had weight zero, which happens when the position sits
        // exactly on a void corner. Fall through to the ring search.
    }
    match nearest_non_void(raster, fx, fy, options, report) {
        Some(h) => {
            report.note(DemAnomaly::VoidFilledFromNearest);
            h
        }
        None => {
            report.note(DemAnomaly::VoidUnfilled);
            options.void_default_m
        }
    }
}

/// The nearest non-void sample to fractional index `(fx, fy)`, searched ring by ring.
///
/// The rings are walked in a fixed order — south edge west to east, then north edge, then
/// west edge, then east edge — so that two samples at the same distance always resolve to
/// the same one. `_report` is threaded through for symmetry with the other helpers and so
/// that a future finer-grained count needs no signature change.
fn nearest_non_void(
    raster: &DemRaster,
    fx: f64,
    fy: f64,
    options: &DemOptions,
    _report: &mut DemReport,
) -> Option<f64> {
    if raster.ncols == 0 || raster.nrows == 0 {
        return None;
    }
    let cx = fx
        .round()
        .clamp(0.0, f64::from(raster.ncols.saturating_sub(1))) as i64;
    let cy = fy
        .round()
        .clamp(0.0, f64::from(raster.nrows.saturating_sub(1))) as i64;
    if let Some(h) = sample_signed(raster, cx, cy) {
        return Some(h);
    }
    for r in 1..=i64::from(options.void_search_cells) {
        for dx in -r..=r {
            if let Some(h) = sample_signed(raster, cx + dx, cy - r) {
                return Some(h);
            }
        }
        for dx in -r..=r {
            if let Some(h) = sample_signed(raster, cx + dx, cy + r) {
                return Some(h);
            }
        }
        for dy in -r + 1..r {
            if let Some(h) = sample_signed(raster, cx - r, cy + dy) {
                return Some(h);
            }
        }
        for dy in -r + 1..r {
            if let Some(h) = sample_signed(raster, cx + r, cy + dy) {
                return Some(h);
            }
        }
    }
    None
}

/// [`DemRaster::sample_at`] for signed indices: outside the raster is `None`.
fn sample_signed(raster: &DemRaster, ix: i64, iy: i64) -> Option<f64> {
    if ix < 0 || iy < 0 {
        return None;
    }
    raster.sample_at(u32::try_from(ix).ok()?, u32::try_from(iy).ok()?)
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Reads a DEM raster from disk, choosing the reader from the file's extension.
///
/// # Errors
///
/// [`WorldError::Io`] if the file cannot be read, [`WorldError::InvalidParameter`] if the
/// extension names no format this module reads or an `.hgt` file's name does not carry
/// its tile corner, and [`WorldError::Malformed`] for a raster this module cannot parse.
pub fn read_dem(path: impl AsRef<Path>, options: &DemOptions) -> Result<(DemRaster, DemReport)> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let format = DemFormat::from_path(path).ok_or_else(|| WorldError::InvalidParameter {
        parameter: "dem".to_string(),
        problem: format!(
            "{} has no extension this module reads; expected .hgt for SRTM or \
             .asc/.arc/.aai/.grd/.txt for an ASCII grid",
            path.display()
        ),
    })?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    read_dem_bytes(&bytes, &name, format, options)
}

/// Reads a DEM raster already in memory.
///
/// `source_id` is what the report calls it, and for [`DemFormat::SrtmHgt`] it must also
/// be the tile's file name, because that name is the tile's only georeferencing.
///
/// # Errors
///
/// As [`read_dem`], without the I/O case.
pub fn read_dem_bytes(
    bytes: &[u8],
    source_id: &str,
    format: DemFormat,
    options: &DemOptions,
) -> Result<(DemRaster, DemReport)> {
    let digest = {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        hasher.finalize()
    };
    let mut report = DemReport {
        source_id: source_id.to_string(),
        source_sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
        source_bytes: bytes.len() as u64,
        format: Some(format),
        source: Some(options.source),
        ..DemReport::default()
    };
    let raster = match format {
        DemFormat::SrtmHgt => {
            let corner =
                parse_srtm_tile(source_id).ok_or_else(|| WorldError::InvalidParameter {
                    parameter: "dem".to_string(),
                    problem: format!(
                        "an .hgt tile carries no header, so its south-west corner comes \
                         from its file name, and {source_id:?} is not of the form \
                         N40W074.hgt; read it with read_srtm_hgt and state the corner"
                    ),
                })?;
            let raster = read_srtm_hgt(bytes, corner)?;
            // Count the voids the reader turned into `None` before anything is resampled.
            // A sample outside the plausible elevation range was turned into a void by the
            // reader too, so it is counted here rather than separately.
            report.note_n(DemAnomaly::SourceVoid, raster.void_count());
            raster
        }
        DemFormat::EsriAsciiGrid => {
            let text = core::str::from_utf8(bytes).map_err(|e| WorldError::Malformed {
                offset: e.valid_up_to(),
                problem: format!("an ASCII grid must be UTF-8 text: {e}"),
            })?;
            let raster = read_ascii_grid(text, options.ascii_grid_crs, &mut report)?;
            report.note_n(DemAnomaly::SourceVoid, raster.void_count());
            raster
        }
    };
    report.raster_size = (raster.ncols, raster.nrows);
    report.source_voids = raster.void_count();
    report.raster_bbox = raster.geodetic_bbox();
    Ok((raster, report))
}

/// Reads a DEM and resamples it onto the grid `options` describes.
///
/// # Errors
///
/// As [`read_dem`] and [`resample`].
pub fn import_terrain(
    path: impl AsRef<Path>,
    options: &DemOptions,
) -> Result<(Terrain, DemReport)> {
    let (raster, mut report) = read_dem(path, options)?;
    let terrain = resample(&raster, options, &mut report)?;
    Ok((terrain, report))
}

/// Reads a DEM and resamples it onto the grid that covers `world`.
///
/// The world supplies the frame — its origin and its bounding box — so a caller does not
/// have to restate them and cannot get them wrong.
///
/// # Errors
///
/// As [`import_terrain`].
pub fn import_terrain_for_world(
    world: &World,
    path: impl AsRef<Path>,
    options: &DemOptions,
) -> Result<(Terrain, DemReport)> {
    let options = options.clone().for_world(world);
    import_terrain(path, &options)
}

// ---------------------------------------------------------------------------
// Attaching and draping
// ---------------------------------------------------------------------------

/// Which geometry [`with_terrain`] lifts onto the DEM.
///
/// Every flag is off by default, so attaching a terrain grid does not move a single
/// coordinate: the world's ground height comes from the DEM
/// ([`World::ground_height_at`]) while the geometry stays where the import put it. That
/// is what keeps an existing scenario's lane geometry — though not its content hash,
/// which now covers the grid — unchanged.
///
/// # What draping means, exactly
///
/// `z_new = z_old + dem(x, y)`: the DEM height is added to whatever `z` the object
/// already had. For a world whose ground is the plane `z = 0` — every OSM import, every
/// procedural world — `z_old` is zero and the result is the DEM height, which is the
/// intent. For a world that already carries real elevations, such as a SUMO network with
/// `z` in its lane shapes, adding them is wrong, so [`with_terrain`] counts every point
/// that was already off the zero plane in [`DrapeReport::points_already_elevated`] and
/// the caller can see that the drape was not applicable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DrapeOptions {
    /// Lift every lane centreline point.
    pub lanes: bool,
    /// Lift every junction position and shape vertex, and every crossing end.
    pub junctions: bool,
    /// Lift every building footprint, and with it its base.
    pub buildings: bool,
    /// Lift every site, so an RSU mast stands on the ground.
    pub sites: bool,
    /// Lift every signal head.
    pub signal_heads: bool,
}

impl DrapeOptions {
    /// Nothing is lifted: the flat case, and the default.
    pub const fn none() -> Self {
        Self {
            lanes: false,
            junctions: false,
            buildings: false,
            sites: false,
            signal_heads: false,
        }
    }

    /// Everything is lifted.
    pub const fn all() -> Self {
        Self {
            lanes: true,
            junctions: true,
            buildings: true,
            sites: true,
            signal_heads: true,
        }
    }

    /// True if nothing at all is to be lifted.
    pub const fn is_none(self) -> bool {
        !(self.lanes || self.junctions || self.buildings || self.sites || self.signal_heads)
    }
}

/// What the drape did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrapeReport {
    /// Points whose `z` the DEM changed.
    pub points_lifted: u64,
    /// Points that fell outside the terrain grid and kept their own `z`.
    pub points_outside_grid: u64,
    /// Points that were already off the `z = 0` plane before the drape — the count that
    /// says the drape's assumption did not hold.
    pub points_already_elevated: u64,
    /// Lanes rebuilt.
    pub lanes: u64,
    /// Junctions moved.
    pub junctions: u64,
    /// Buildings rebuilt.
    pub buildings: u64,
    /// Sites moved.
    pub sites: u64,
    /// Signal heads moved.
    pub heads: u64,
}

/// Attaches a terrain grid to a world, optionally lifting its geometry onto it.
///
/// The returned world is a new one: its terrain is `terrain`, its provenance carries the
/// DEM resampling transformation and the layer's licence and attribution
/// (04-models.md §1.4, §1.5), and its content hash is recomputed, because the grid is part
/// of the hashed content.
///
/// # Errors
///
/// [`WorldError::InvalidParameter`] if the world already has a terrain grid and a drape
/// was asked for — adding one DEM's heights to another's would be silently wrong — and
/// whatever the geometry itself rejects when it is rebuilt.
pub fn with_terrain(
    world: &World,
    terrain: Terrain,
    drape: &DrapeOptions,
    report: &DemReport,
) -> Result<(World, DrapeReport)> {
    if world.terrain.is_some() && !drape.is_none() {
        return Err(WorldError::InvalidParameter {
            parameter: "drape".to_string(),
            problem: "this world already has a terrain grid, so its geometry may already \
                      be draped; draping again would add one DEM's heights to another's"
                .to_string(),
        });
    }
    let mut parts = world.to_parts();
    let mut terrain = terrain;
    let source_symbol = parts.symbols.intern(
        report
            .source
            .and_then(DemSource::model_id)
            .unwrap_or("world/terrain/unstated"),
    );
    terrain.source = Some(source_symbol);

    let mut drape_report = DrapeReport::default();
    let lift = |p: Vec3, r: &mut DrapeReport| -> Vec3 {
        if p.z.abs() > Q_HEIGHT_M {
            r.points_already_elevated += 1;
        }
        match terrain.height_at(p.x, p.y) {
            Some(h) => {
                r.points_lifted += 1;
                Vec3::new(p.x, p.y, p.z + h)
            }
            None => {
                r.points_outside_grid += 1;
                p
            }
        }
    };

    let mut lanes = parts.roads.lanes().to_vec();
    // An edge carries no geometry of its own, so a drape never touches one.
    let edges = parts.roads.edges().to_vec();
    let mut junctions = parts.roads.junctions().to_vec();
    let connections = parts.roads.connections().to_vec();
    let mut crossings = parts.roads.crossings().to_vec();

    if drape.lanes {
        let mut rebuilt = Vec::with_capacity(lanes.len());
        for lane in &lanes {
            let centreline: Vec<Vec3> = lane
                .centreline
                .iter()
                .map(|p| lift(*p, &mut drape_report))
                .collect();
            rebuilt.push(crate::model::Lane::new(
                lane.id,
                lane.edge,
                lane.junction,
                lane.index,
                lane.kind,
                centreline,
                lane.width_m,
                lane.speed_limit_mps,
                lane.allowed,
            )?);
            drape_report.lanes += 1;
        }
        lanes = rebuilt;
    }
    if drape.junctions {
        for j in &mut junctions {
            j.position = lift(j.position, &mut drape_report);
            for p in &mut j.shape {
                *p = lift(*p, &mut drape_report);
            }
            drape_report.junctions += 1;
        }
        for c in &mut crossings {
            c.from = lift(c.from, &mut drape_report);
            c.to = lift(c.to, &mut drape_report);
        }
    }
    let buildings = if drape.buildings {
        let mut rebuilt = Vec::with_capacity(parts.buildings.len());
        for b in &parts.buildings {
            let footprint: Vec<Vec3> = b
                .footprint
                .iter()
                .map(|p| lift(*p, &mut drape_report))
                .collect();
            let holes: Vec<Vec<Vec3>> = b
                .holes
                .iter()
                .map(|h| h.iter().map(|p| lift(*p, &mut drape_report)).collect())
                .collect();
            let mut lifted = Building::new(
                b.id,
                footprint,
                holes,
                b.height_m,
                b.min_height_m,
                b.material,
                b.height_source,
            )?;
            // `Building::new` is a constructor, not a copy: it derives the base from the
            // footprint and resets the fields it does not take. The three it resets are
            // restored here so a drape is a change of height and nothing else.
            lifted.levels = b.levels;
            lifted.lod = b.lod;
            lifted.name = b.name;
            rebuilt.push(lifted);
            drape_report.buildings += 1;
        }
        rebuilt
    } else {
        parts.buildings.clone()
    };
    let mut sites = parts.sites.clone();
    if drape.sites {
        for s in &mut sites {
            s.position = lift(s.position, &mut drape_report);
            drape_report.sites += 1;
        }
    }
    let mut signals = parts.signals.clone();
    if drape.signal_heads {
        for plan in &mut signals {
            for head in &mut plan.heads {
                head.position = lift(head.position, &mut drape_report);
                drape_report.heads += 1;
            }
        }
    }

    let roads = RoadNetwork::new(lanes, edges, junctions, connections, crossings)?;

    let mut provenance = parts.provenance.clone();
    if let Some(id) = report.source.and_then(DemSource::model_id) {
        provenance
            .tool_versions
            .insert(id.to_string(), MODEL_VERSION.to_string());
    }
    provenance.record(
        Transformation::new("dem-resample")
            .with("source", report.source.map_or("unstated", DemSource::label))
            .with("format", report.format.map_or("unknown", DemFormat::label))
            .with("source_sha256", report.source_sha256.clone())
            .with("interpolation", terrain.interpolation.label())
            .with("cell_x_m", terrain.cell_x_m)
            .with("cell_y_m", terrain.cell_y_m)
            .with("nx", terrain.nx)
            .with("ny", terrain.ny)
            .with("origin_x_m", terrain.origin_x_m)
            .with("origin_y_m", terrain.origin_y_m)
            .with("source_voids", report.source_voids)
            .with("samples_repaired", report.samples_repaired),
    );
    if !drape.is_none() {
        provenance.record(
            Transformation::new("dem-drape")
                .with("rule", "z_new = z_old + dem(x, y)")
                .with("lanes", drape.lanes)
                .with("junctions", drape.junctions)
                .with("buildings", drape.buildings)
                .with("sites", drape.sites)
                .with("signal_heads", drape.signal_heads)
                .with("points_lifted", drape_report.points_lifted)
                .with("points_outside_grid", drape_report.points_outside_grid)
                .with(
                    "points_already_elevated",
                    drape_report.points_already_elevated,
                ),
        );
    }
    let layer = report.source.unwrap_or_default().layer_licence();
    if !provenance.layers.iter().any(|l| l.layer == layer.layer) {
        provenance.layers.push(layer);
    }
    if report.source == Some(DemSource::CopernicusGlo30) {
        for note in [COPERNICUS_LIABILITY, COPERNICUS_ATTRIBUTION_UNMODIFIED] {
            let note = note.to_string();
            if !provenance.notes.contains(&note) {
                provenance.notes.push(note);
            }
        }
    }
    if report.source == Some(DemSource::Unstated) || report.source.is_none() {
        let note = "The terrain layer's provenance was not stated by the caller, so no \
                    licence or attribution is recorded for it."
            .to_string();
        if !provenance.notes.contains(&note) {
            provenance.notes.push(note);
        }
    }

    let world = World::builder(parts.origin)
        .roads(roads)
        .buildings(buildings)
        .landuse(core::mem::take(&mut parts.landuse))
        .signals(signals)
        .sites(sites)
        .default_env(parts.default_env)
        .symbols(parts.symbols.clone())
        .provenance(provenance)
        .index_options(parts.index_options)
        .terrain(terrain)
        .build()?;
    Ok((world, drape_report))
}

// ---------------------------------------------------------------------------
// Model cards
// ---------------------------------------------------------------------------

/// The model card of `world/terrain/srtm-30` (03-interfaces.md §12).
pub fn srtm_card() -> ModelCard {
    let source = Source {
        kind: SourceKind::Dataset,
        reference: "NASA Earthdata SRTMGL1 (1 arc-second), via 04-models.md §1.4 (R10 §A8)"
            .to_string(),
        accessed: None,
        note: Some(
            "Openly shared without restriction under EOSDIS data-use guidance; citation \
             requested, not legally required. The vertical accuracy is UNVERIFIED: the \
             Earthdata page defers to Rodriguez et al. 2006, which was not retrieved."
                .to_string(),
        ),
    };
    dem_card(SRTM_MODEL_ID, source, "NASA SRTMGL1 .hgt tiles")
}

/// The model card of `world/terrain/copernicus-glo-30` (03-interfaces.md §12).
pub fn copernicus_card() -> ModelCard {
    let source = Source {
        kind: SourceKind::Dataset,
        reference: "Copernicus DEM COP-DEM-GLO-30-F, via 04-models.md §1.4 (R10 §A9)".to_string(),
        accessed: None,
        note: Some(format!(
            "Free of charge with reproduction, distribution, adaptation and modification \
             rights, worldwide and unlimited in time. Resampling is a modification, so the \
             attribution recorded is the modified-data wording: {COPERNICUS_ATTRIBUTION_MODIFIED} \
             A redistributor must also add: {COPERNICUS_LIABILITY}. WorldDEM-10 is excluded. \
             The distribution is a GeoTIFF this crate does not parse; the supported path is \
             an ASCII-grid export."
        )),
    };
    dem_card(
        COPERNICUS_MODEL_ID,
        source,
        "Copernicus COP-DEM-GLO-30 exported as an ASCII grid",
    )
}

/// Every DEM importer's card, which differs only in its dataset citation.
fn dem_card(id: &str, dataset: Source, what: &str) -> ModelCard {
    let models = |what: &str| Source::new(SourceKind::Code, format!("04-models.md §1.4 ({what})"));
    let todo = |name: &str, unit: &str, default: serde_json::Value, plan: &str| Parameter {
        name: name.to_string(),
        unit: unit.to_string(),
        default,
        range: None,
        source: Source::todo_calibrate(format!("{id} {name}")),
        calibration: Some(plan.to_string()),
    };
    let parameters = vec![
        Parameter::new(
            "cell_m",
            "m",
            DEM_POST_SPACING_M.into(),
            models("both DEMs are 30 m posts, so 30 m is the resolution the data carries"),
        ),
        Parameter::new(
            "interpolation",
            "-",
            "bilinear".into(),
            models("\"resampled onto the world grid with bilinear interpolation\""),
        ),
        todo(
            "margin_m",
            "m",
            DEM_POST_SPACING_M.into(),
            "one cell, so a link that leaves the road network still samples the grid; \
             measure how far Phase 3 endpoints actually stray outside the network's \
             bounding box and set the margin from that distribution",
        ),
        todo(
            "void_search_cells",
            "-",
            8.into(),
            "8 cells is 240 m at 30 m posts, chosen so that an SRTM water void is \
             crossed in one hop; measure the void-run-length distribution of the Phase 3 \
             tiles and set the radius to its 99th percentile",
        ),
        todo(
            "void_default_m",
            "m",
            0.0.into(),
            "the height a sample takes when the search finds nothing. Zero is the world's \
             own datum and is visibly wrong rather than plausibly wrong; replace it with \
             the tile's median elevation once the void statistics of the Phase 3 tiles are \
             measured",
        ),
        todo(
            "min_plausible_elevation_m",
            "m",
            MIN_PLAUSIBLE_ELEVATION_M.into(),
            "a range check, not a model: below the Dead Sea shore (about -430 m) and above \
             Everest (8 849 m) a sample is a byte-order mistake rather than terrain. \
             Replace both bounds with the published min and max of whichever DEM is in use",
        ),
        todo(
            "max_plausible_elevation_m",
            "m",
            MAX_PLAUSIBLE_ELEVATION_M.into(),
            "the upper half of the same range check: Everest is 8 849 m, so 9 000 m leaves \
             headroom over any terrestrial sample while still rejecting a byte-order \
             mistake, which turns a 100 m hill into tens of thousands of metres. Replace \
             it with the published maximum of whichever DEM is in use, taken from the \
             dataset's own statistics rather than from this module",
        ),
    ];
    ModelCard {
        tier: vec![Tier::Medium, Tier::High],
        equations: vec![
            Equation::new(
                "bilinear resampling",
                "h(x, y) = Σ_k h_k · w_k over the four surrounding source samples, \
                 w = (1−tx)(1−ty), tx(1−ty), (1−tx)ty, tx·ty",
            ),
            Equation::new(
                "void repair",
                "h = Σ_present h_k·w_k / Σ_present w_k, falling back to the nearest \
                 non-void sample within void_search_cells",
            ),
            Equation::new(
                "srtm tile georeferencing",
                "sample (ix, iy) at (lon0 + ix/(n−1), lat0 + iy/(n−1)) degrees, (lat0, lon0) \
                 from the file name, n from the byte count",
            ),
        ],
        parameters,
        assumptions: vec![
            format!("The raster holds {what}."),
            "Heights are metres above the DEM's own datum, and that datum is taken to be \
             the world's z = 0. Neither DEM's geoid is converted to an ellipsoidal height, \
             and the offset between them (tens of metres in places) is not modelled."
                .to_string(),
            "A void is missing data, not sea level: it is interpolated or filled from a \
             neighbour, never read as 0 m, unless the search finds nothing at all."
                .to_string(),
        ],
        limitations: vec![
            "The GeoTIFF and DT2 distributions are not parsed; a GeoTIFF DEM must be \
             converted to an ASCII grid first (`gdal_translate -of AAIGrid`)."
                .to_string(),
            "The vertical datum is not converted, so an absolute height is only as good as \
             the source's datum matching the world's."
                .to_string(),
            "A geodetic raster is sampled through the world's local tangent plane, so the \
             resampling error grows with distance from the world origin exactly as the \
             projection's does."
                .to_string(),
        ],
        ignores: vec![
            "Slope-aware or bicubic resampling: 04-models.md §1.4 specifies bilinear and \
             the grid records that it was used."
                .to_string(),
            "Buildings and vegetation: both DEMs are surface-ish models whose treatment of \
             canopy and roofs is not separated here."
                .to_string(),
        ],
        sources: vec![dataset],
        validation: Validation::new(ValidationStatus::UnitTested),
        determinism: Determinism {
            uses_rng: false,
            rng_domains: Vec::new(),
        },
        ..ModelCard::new(
            id,
            Family::World,
            MODEL_VERSION,
            "Reads a raster digital elevation model and resamples it bilinearly onto the \
             world's own metre grid, so that the world's ground height comes from measured \
             terrain instead of the z = 0 plane.",
        )
    }
}

/// Every model card this module publishes.
pub fn model_cards() -> Vec<ModelCard> {
    vec![srtm_card(), copernicus_card()]
}
