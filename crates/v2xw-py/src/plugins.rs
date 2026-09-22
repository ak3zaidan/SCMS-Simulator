//! `v2xw.plugins` — how a model written in Python gets called by the engine.
//!
//! 03-interfaces.md §15 says a researcher writes a class, gives it a model card and the
//! engine calls it. This module is the Rust half of that sentence: a Python object comes
//! in, a Rust trait object goes out, and the trait object is what the engine already knows
//! how to call. Nothing in the engine learns that a model is Python.
//!
//! # The two families implemented here, and why these two
//!
//! * **[`PyCarFollowing`]** implements `v2xw_mobility::CarFollowing`, an existing engine
//!   seam. `accel` is *pure* — it takes views and returns a number, draws no random values
//!   and has no context — so it is the family where the shape can be proved end to end with
//!   nothing else in the way. It is also, per §15's rule P2, a **hot path**: the engine
//!   calls it once per vehicle per mobility step, which means one GIL acquisition per
//!   vehicle per step. A run with a Python car-following model must be marked
//!   `python-hot-path` in its manifest with an expected slowdown, and this crate cannot mark
//!   it, because the manifest is assembled inside `Engine::build` before a plug-in is
//!   attached. That is the crate's largest owed change and its README says so.
//! * **[`PyDetector`]** implements [`Detector`], which is declared *in this crate* because
//!   no crate declares it yet — 07-threats-and-detection.md specifies the family and the
//!   `v2xw-threat` crate is still a stub. It is the **batched** family of §15: a whole
//!   window of messages crosses as one Arrow record batch and the plug-in returns a list of
//!   observations, so the per-call cost is amortised over the batch rather than paid per
//!   message. When the detector seam lands for real, this trait moves and the adapter
//!   follows it unchanged.
//!
//! # What a Python plug-in may not do
//!
//! A plug-in is a *function*, and the engine's determinism rests on it being one:
//!
//! 1. **No random numbers of its own.** Randomness comes from `RngRegistry` streams keyed
//!    by `(RngDomain, EntityRef)`, so that a draw does not depend on what else was drawn
//!    first (ADR 0004 §3). A plug-in that calls `random.gauss` has a stream the engine does
//!    not know about, seeded from the OS, and the run is not reproducible. Neither family
//!    here is even given a way to ask for a stream: `accel` takes no context, and a
//!    detector's context is the read-only [`DetectorCtx`].
//! 2. **No wall clock.** `time.time()`, `datetime.now()` and `time.perf_counter()` are all
//!    outside the simulation. Time is the `t` on the batch.
//! 3. **No platform libm.** `math.exp`, `**` on floats and `numpy`'s ufuncs are the C
//!    library and can differ in the low bits between machines. Use [`crate::mathmod`],
//!    which is the engine's own pure-Rust libm.
//! 4. **No dependence on iteration order of a `set` or a `dict` keyed by anything hashed.**
//!    Python's string hashing is randomised per process unless `PYTHONHASHSEED` is fixed.
//! 5. **No mutable state carried between calls that the engine cannot see**, because the
//!    engine reorders and parallelises phases.
//!
//! Rules 1 to 4 are checkable and [`crate::conformance`] checks them. Rule 5 is not
//! checkable in general and is caught by the two-run digest comparison, which is why that
//! check is the one the kit will not let a plug-in skip.
//!
//! # Errors inside a plug-in
//!
//! `CarFollowing::accel` returns `f64` with no error channel, so a Python exception has
//! nowhere to go. Swallowing it and returning `0.0` would put a vehicle into free
//! acceleration and produce a run that looks fine and is wrong, so the adapter does the
//! opposite: it stores the traceback and returns `NaN`. A `NaN` acceleration fails the
//! writer's quantisation post-condition (build decision D9) at the first record it reaches,
//! the run stops, and [`PyCarFollowing::last_error`] says why. A number that is visibly
//! not a number is a better failure than a plausible one.

use std::sync::Mutex;

use arrow::array::RecordBatch;
use arrow::pyarrow::IntoPyArrow;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use v2xw_core::card::{Family, ModelCard};
use v2xw_core::geom::Dims;
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;
use v2xw_core::weather::WeatherState;
use v2xw_mobility::classes::VehicleClass;
use v2xw_mobility::traits::CarFollowing;
use v2xw_mobility::views::{DriverProfile, LaneView, LeaderView, VehicleView};

use crate::err;

/// The name a serde-tagged enum writes, without the JSON quotes.
///
/// Used for the class, lane-kind, weather-kind and surface fields the views carry. Going
/// through the serde derive rather than writing a match keeps the Python-visible spelling
/// (`"passenger"`, not `"Passenger"`) equal to the spelling in every record and every
/// scenario file, and keeps it equal after someone adds a variant.
fn enum_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// The ego vehicle as a Python car-following model sees it.
///
/// A flat record of scalars rather than a nested mirror of `VehicleView`: every field a
/// longitudinal model reads is one attribute access, and the driver parameters — which are
/// the model's *own* calibration, drawn per driver — are on the same object as the
/// kinematics that they act on.
#[pyclass(name = "Ego", module = "v2xw.plugins", frozen, get_all)]
#[derive(Debug, Clone)]
pub struct PyEgo {
    /// The actor's dense id.
    pub actor: u32,
    /// Its class, e.g. `"passenger"`, `"bus"`, `"truck"`.
    pub vehicle_class: String,
    /// The lane it is on.
    pub lane: u32,
    /// Its index within the edge; `0` is rightmost.
    pub lane_index: u8,
    /// Arc length of the front bumper along the lane, metres.
    pub s_m: f64,
    /// Lateral offset from the centreline, metres, positive to the left.
    pub lateral_m: f64,
    /// Speed along the lane, m/s.
    pub speed_mps: f64,
    /// The acceleration the previous step applied, m/s².
    pub accel_mps2: f64,
    /// Heading in the ENU frame, radians.
    pub heading_rad: f64,
    /// Body length, metres.
    pub length_m: f64,
    /// Body width, metres.
    pub width_m: f64,
    /// Body height, metres.
    pub height_m: f64,
    /// Desired free-road speed `v0`, m/s.
    pub desired_speed_mps: f64,
    /// Comfortable maximum acceleration `a`, m/s².
    pub max_accel_mps2: f64,
    /// Comfortable deceleration `b`, m/s², a positive magnitude.
    pub comfort_decel_mps2: f64,
    /// Desired time headway `T`, seconds.
    pub time_headway_s: f64,
    /// Standstill distance `s0`, metres.
    pub min_gap_m: f64,
}

#[pymethods]
impl PyEgo {
    fn __repr__(&self) -> String {
        format!(
            "<Ego a{} {} v={:.2} s={:.2}>",
            self.actor, self.vehicle_class, self.speed_mps, self.s_m
        )
    }
}

impl From<&VehicleView> for PyEgo {
    fn from(v: &VehicleView) -> Self {
        Self {
            actor: v.actor.index(),
            vehicle_class: enum_name(&v.class),
            lane: v.lane.index(),
            lane_index: v.lane_index,
            s_m: v.s_m,
            lateral_m: v.lateral_m,
            speed_mps: v.speed_mps,
            accel_mps2: v.accel_mps2,
            heading_rad: v.heading_rad,
            length_m: v.dims.length_m,
            width_m: v.dims.width_m,
            height_m: v.dims.height_m,
            desired_speed_mps: v.driver.desired_speed_mps,
            max_accel_mps2: v.driver.max_accel_mps2,
            comfort_decel_mps2: v.driver.comfort_decel_mps2,
            time_headway_s: v.driver.time_headway_s,
            min_gap_m: v.driver.min_gap_m,
        }
    }
}

/// What is ahead: a vehicle, or a virtual obstacle standing in for a stop line, a red
/// signal, a junction to yield at or a bend to slow into.
#[pyclass(name = "Leader", module = "v2xw.plugins", frozen, get_all)]
#[derive(Debug, Clone)]
pub struct PyLeader {
    /// Net gap: the leader's rear to the ego's front, metres, length already subtracted.
    pub gap_m: f64,
    /// The leader's speed, m/s — zero for a stopped virtual obstacle.
    pub speed_mps: f64,
    /// The leader's acceleration, m/s².
    pub accel_mps2: f64,
    /// The leader's length, metres — zero for a virtual obstacle, which has no body.
    pub length_m: f64,
    /// False for a virtual obstacle. A model that brakes differently for a stop line than
    /// for a car is the reason this is visible.
    pub is_vehicle: bool,
}

#[pymethods]
impl PyLeader {
    fn __repr__(&self) -> String {
        format!(
            "<Leader gap={:.2} v={:.2} vehicle={}>",
            self.gap_m, self.speed_mps, self.is_vehicle
        )
    }
}

impl From<&LeaderView> for PyLeader {
    fn from(l: &LeaderView) -> Self {
        Self {
            gap_m: l.gap_m,
            speed_mps: l.speed_mps,
            accel_mps2: l.accel_mps2,
            length_m: l.length_m,
            is_vehicle: l.vehicle.is_some(),
        }
    }
}

/// The lane the ego is on.
#[pyclass(name = "Lane", module = "v2xw.plugins", frozen, get_all)]
#[derive(Debug, Clone)]
pub struct PyLane {
    /// The lane's dense id.
    pub id: u32,
    /// What the lane is for, e.g. `"driving"`.
    pub kind: String,
    /// Its speed limit, m/s.
    pub speed_limit_mps: f64,
    /// Its width, metres.
    pub width_m: f64,
    /// Its centreline length, metres.
    pub length_m: f64,
}

#[pymethods]
impl PyLane {
    fn __repr__(&self) -> String {
        format!("<Lane l{} limit={:.1}>", self.id, self.speed_limit_mps)
    }
}

impl From<&LaneView> for PyLane {
    fn from(l: &LaneView) -> Self {
        Self {
            id: l.id.index(),
            kind: enum_name(&l.kind),
            speed_limit_mps: l.speed_limit_mps,
            width_m: l.width_m,
            length_m: l.length_m,
        }
    }
}

/// The weather, as a behavioural model sees it.
#[pyclass(name = "Weather", module = "v2xw.plugins", frozen, get_all)]
#[derive(Debug, Clone)]
pub struct PyWeather {
    /// What the weather is, e.g. `"clear"`, `"rain"`, `"snow"`.
    pub kind: String,
    /// How hard it is doing it, on `[0, 1]`.
    pub intensity: f64,
    /// Meteorological visibility, metres. Unrestricted visibility arrives as `inf`.
    pub visibility_m: f64,
    /// The road surface, which need not follow from `kind`.
    pub surface: String,
}

#[pymethods]
impl PyWeather {
    fn __repr__(&self) -> String {
        format!("<Weather {} {:.2}>", self.kind, self.intensity)
    }
}

impl From<&WeatherState> for PyWeather {
    fn from(w: &WeatherState) -> Self {
        Self {
            kind: enum_name(&w.kind),
            intensity: w.intensity,
            visibility_m: w.visibility_m,
            surface: enum_name(&w.surface),
        }
    }
}

/// Validates a model card supplied as a Python `dict` and returns it normalised.
///
/// Every plug-in must have one (03-interfaces.md §12): a card is what makes a number in an
/// output traceable to the equation and the source it came from. The check here is
/// `ModelCard::validate` itself, not a re-implementation of it, so a card that this accepts
/// is a card the registry accepts.
///
/// # Errors
/// `V2xwError` if the dictionary is not a card, or if the card does not validate — with the
/// validator's own message, which names the field.
#[pyfunction]
pub fn validate_card<'py>(
    py: Python<'py>,
    card: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let parsed = card_from_py(card)?;
    parsed.validate().map_err(|e| {
        err::V2xwError::new_err(format!("model card {}: {}", parsed.id, err::chain(&e)))
    })?;
    crate::scenario::to_py_dict(py, "model card", &parsed)
}

/// Every parameter whose default is uncalibrated *and* has no calibration plan.
///
/// The rule is: every default cites a source, and a default with no source is marked
/// `todo-calibrate` **with** a plan for calibrating it. Registry rule R1 already refuses a
/// card that breaks the second half, so this returns the same list the registry would
/// refuse on — surfaced as data, for a reviewer, rather than as an error.
#[pyfunction]
pub fn uncited_parameters(card: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    let parsed = card_from_py(card)?;
    Ok(parsed
        .parameters
        .iter()
        .filter(|p| {
            p.source.kind == v2xw_core::card::SourceKind::TodoCalibrate
                && p.calibration.as_deref().unwrap_or("").trim().is_empty()
        })
        .map(|p| p.name.clone())
        .collect())
}

/// Reads a `ModelCard` out of a Python object, which may be a `dict` or a JSON string.
fn card_from_py(card: &Bound<'_, PyAny>) -> PyResult<ModelCard> {
    let json: String = if let Ok(s) = card.extract::<String>() {
        s
    } else {
        card.py()
            .import("json")?
            .call_method1("dumps", (card,))?
            .extract()?
    };
    serde_json::from_str(&json).map_err(|e| err::json("model card", e))
}

/// A car-following model written in Python, wearing the engine's trait.
///
/// The Python object must supply `accel(ego, leader, lane, weather) -> float`, where
/// `leader` is `None` on a free road. See the module note for what it may not do, and for
/// why a raised exception becomes a `NaN` rather than a zero.
pub struct PyCarFollowing {
    object: Py<PyAny>,
    card: ModelCard,
    /// The last traceback, if a call raised. Behind a mutex because the registry requires
    /// `Send + Sync`, and because a phase-parallel map may call this from several threads.
    last_error: Mutex<Option<String>>,
}

impl std::fmt::Debug for PyCarFollowing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyCarFollowing")
            .field("id", &self.card.id)
            .finish_non_exhaustive()
    }
}

impl PyCarFollowing {
    /// Adapts a Python object, validating its card first.
    ///
    /// The card is read from the object's `card` attribute, which §15 writes as a class
    /// attribute. Validation happens here, at attach time, rather than at the first call:
    /// a run that is going to be refused for a malformed card should be refused before it
    /// produces half a recording.
    ///
    /// # Errors
    /// `V2xwError` if the object has no `card`, if the card does not validate, if it is not
    /// in the `mobility/car-following` family, or if the object has no callable `accel`.
    pub fn attach(object: Bound<'_, PyAny>) -> PyResult<Self> {
        let card_obj = object.getattr("card").map_err(|_| {
            err::V2xwError::new_err(
                "a plug-in must carry a model card: set a `card` attribute (03-interfaces.md §12)",
            )
        })?;
        let card = card_from_py(&card_obj)?;
        card.validate().map_err(|e| {
            err::V2xwError::new_err(format!("model card {}: {}", card.id, err::chain(&e)))
        })?;
        // `Family::Mobility` is what the built-in IDM declares: 03-interfaces.md §12's
        // family list has no separate `car-following` entry, so the longitudinal models sit
        // under mobility with the rest of the driving behaviour.
        if card.family != Family::Mobility {
            return Err(err::V2xwError::new_err(format!(
                "model card {} declares family {}, but it is being attached as a \
                 car-following model, whose family is `mobility`; a card's family is the \
                 seam it plugs into",
                card.id, card.family
            )));
        }
        if !object.getattr("accel").is_ok_and(|a| a.is_callable()) {
            return Err(err::V2xwError::new_err(format!(
                "car-following plug-in {} has no callable `accel(ego, leader, lane, weather)`",
                card.id
            )));
        }
        Ok(Self {
            object: object.unbind(),
            card,
            last_error: Mutex::new(None),
        })
    }

    /// The last traceback a call raised, if any.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }

    /// Records a Python error and returns the `NaN` the caller propagates.
    fn fail(&self, e: PyErr) -> f64 {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(e.to_string());
        }
        f64::NAN
    }
}

impl Model for PyCarFollowing {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl CarFollowing for PyCarFollowing {
    fn accel(
        &self,
        ego: &VehicleView,
        leader: Option<&LeaderView>,
        lane: &LaneView,
        w: &WeatherState,
    ) -> f64 {
        Python::with_gil(|py| {
            let args = (
                PyEgo::from(ego),
                leader.map(PyLeader::from),
                PyLane::from(lane),
                PyWeather::from(w),
            );
            match self
                .object
                .bind(py)
                .call_method1("accel", args)
                .and_then(|r| r.extract::<f64>())
            {
                Ok(a) => a,
                Err(e) => self.fail(e),
            }
        })
    }

    fn profile(&self, class: VehicleClass) -> DriverProfile {
        // Defaulted through the trait when the plug-in does not override it, which is the
        // documented behaviour: a model with no per-class calibration of its own borrows
        // §2.1's Kesting 2010 set and the card has to say so.
        let name = enum_name(&class);
        let custom = Python::with_gil(|py| -> Option<DriverProfile> {
            let obj = self.object.bind(py);
            let f = obj.getattr("profile").ok()?;
            if !f.is_callable() {
                return None;
            }
            let d = f.call1((name.as_str(),)).ok()?;
            let get = |k: &str| d.get_item(k).ok()?.extract::<f64>().ok();
            Some(DriverProfile {
                desired_speed_mps: get("desired_speed_mps")?,
                max_accel_mps2: get("max_accel_mps2")?,
                comfort_decel_mps2: get("comfort_decel_mps2")?,
                time_headway_s: get("time_headway_s")?,
                min_gap_m: get("min_gap_m")?,
            })
        });
        custom.unwrap_or_else(|| {
            v2xw_mobility::carfollowing::idm::IdmPreset::Kesting2010.profile(class)
        })
    }
}

/// A Python car-following model, as a handle Python can hold and the conformance kit can
/// drive.
#[pyclass(name = "CarFollowingModel", module = "v2xw.plugins", frozen)]
pub struct PyCarFollowingHandle {
    inner: PyCarFollowing,
}

#[pymethods]
impl PyCarFollowingHandle {
    /// Attaches a Python object as a car-following model.
    #[new]
    fn new(object: Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            inner: PyCarFollowing::attach(object)?,
        })
    }

    /// The model's id.
    #[getter]
    fn id(&self) -> &str {
        &self.inner.card.id
    }

    /// The validated card.
    #[getter]
    fn card<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        crate::scenario::to_py_dict(py, "model card", &self.inner.card)
    }

    /// The last traceback a call raised, if any.
    #[getter]
    fn last_error(&self) -> Option<String> {
        self.inner.last_error()
    }

    /// Calls the model through the **Rust trait object**, exactly as the engine would.
    ///
    /// This is what makes the conformance kit meaningful: it does not call the Python
    /// method directly, it goes `dyn CarFollowing` → adapter → Python, so a change to the
    /// trait or to the adapter is exercised here rather than only in a live run.
    #[pyo3(signature = (ego_speed_mps, gap_m=None, leader_speed_mps=0.0, speed_limit_mps=URBAN_SPEED_LIMIT_MPS, is_vehicle=true))]
    fn accel(
        &self,
        ego_speed_mps: f64,
        gap_m: Option<f64>,
        leader_speed_mps: f64,
        speed_limit_mps: f64,
        is_vehicle: bool,
    ) -> f64 {
        let model: &dyn CarFollowing = &self.inner;
        let (ego, lane, weather) = synthetic_scene(ego_speed_mps, speed_limit_mps);
        let leader = gap_m.map(|gap| {
            let mut l = LeaderView::virtual_obstacle(gap, leader_speed_mps);
            if is_vehicle {
                let mut v = ego;
                v.speed_mps = leader_speed_mps;
                l = LeaderView::of(v, gap);
            }
            l
        });
        model.accel(&ego, leader.as_ref(), &lane, &weather)
    }
}

/// A scene with nothing in it but the ego, its lane and clear weather.
///
/// The conformance kit and the worked example both need a scene to call a longitudinal
/// model against, and neither should have to build a world to get one. Every number in it
/// is a schema default or the Kesting 2010 calibration the trait itself defaults to, so
/// this fixture invents nothing.
fn synthetic_scene(speed_mps: f64, speed_limit_mps: f64) -> (VehicleView, LaneView, WeatherState) {
    let driver =
        v2xw_mobility::carfollowing::idm::IdmPreset::Kesting2010.profile(VehicleClass::Passenger);
    let ego = VehicleView {
        actor: v2xw_core::ids::ActorId::new(0),
        class: VehicleClass::Passenger,
        lane: v2xw_core::ids::LaneId::new(0),
        lane_index: 0,
        s_m: 0.0,
        lateral_m: 0.0,
        speed_mps,
        accel_mps2: 0.0,
        heading_rad: 0.0,
        dims: Dims::new(4.5, 1.8, 1.5),
        driver,
    };
    let lane = LaneView {
        id: v2xw_core::ids::LaneId::new(0),
        kind: v2xw_world::LaneKind::Driving,
        speed_limit_mps,
        width_m: 3.5,
        length_m: 1000.0,
        allowed: v2xw_world::ClassMask::ALL,
    };
    (ego, lane, WeatherState::CLEAR)
}

// ---------------------------------------------------------------------------------------
// The detector family
// ---------------------------------------------------------------------------------------

/// What a detector knows about the moment it is being called in.
///
/// Read-only and deliberately thin. It carries the simulated instant and the node the
/// detector is running on, and **no RNG accessor**: a local misbehaviour detector that
/// needs a random number needs an engine stream keyed by its entity, which is a seam that
/// belongs to the detector crate when that crate exists.
#[pyclass(name = "DetectorCtx", module = "v2xw.plugins", frozen, get_all)]
#[derive(Debug, Clone, Copy)]
pub struct DetectorCtx {
    /// The instant of the batch, nanoseconds of simulated time.
    pub t_ns: u64,
    /// The node the detector is running on.
    pub node: u32,
}

/// One thing a detector concluded about one subject.
///
/// Flat, and quantised at the writer: `confidence` sits on the probability grid of build
/// decision D9 before it leaves the plug-in, so a detector cannot emit a float off its
/// grid by forgetting to round it.
#[pyclass(name = "Observation", module = "v2xw.plugins", frozen, get_all)]
#[derive(Debug, Clone)]
pub struct Observation {
    /// The node the observation is about.
    pub subject: u32,
    /// The check that fired, e.g. `"position-plausibility"`.
    pub kind: String,
    /// How sure the detector is, on `[0, 1]`, on the probability grid.
    pub confidence: f64,
    /// The instant it concluded this, nanoseconds.
    pub t_ns: u64,
}

#[pymethods]
impl Observation {
    /// Builds an observation, quantising the confidence onto the probability grid.
    #[new]
    fn new(subject: u32, kind: String, confidence: f64, t_ns: u64) -> PyResult<Self> {
        if !(confidence.is_finite() && (0.0..=1.0).contains(&confidence)) {
            return Err(err::V2xwError::new_err(format!(
                "confidence must be a finite number on [0, 1], got {confidence}"
            )));
        }
        Ok(Self {
            subject,
            kind,
            confidence: v2xw_core::math::quantize_to(confidence, PROBABILITY_QUANTUM),
            t_ns,
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "<Observation n{} {} p={:.4}>",
            self.subject, self.kind, self.confidence
        )
    }
}

/// The grid a probability is exported on (build decision D9).
pub const PROBABILITY_QUANTUM: f64 = 1e-6;

/// The speed limit the synthetic conformance scene uses: 50 km/h, in m/s.
///
/// The urban default of every European scenario preset, rounded to the six digits the
/// metre-per-second grid can hold. It is a fixture value and nothing is calibrated against
/// it; a real lane's limit comes from the imported world.
pub const URBAN_SPEED_LIMIT_MPS: f64 = 13.888_889;

/// A local misbehaviour detector, batched per step.
///
/// **Declared here as a hook.** 07-threats-and-detection.md specifies this family and
/// `v2xw-threat` is where it belongs; that crate is a stub, so the trait is defined against
/// what §15 publishes and moves when the real one lands. Its shape is the one §15 fixes: a
/// whole window of messages arrives as one Arrow record batch, so a Python implementation
/// pays one call per window instead of one per message.
pub trait Detector: Model {
    /// What this detector concludes about the messages in `batch`.
    ///
    /// `batch` is the `node.rx` table for the window: one row per received message, with
    /// the fields the receiving node can actually see. A detector that reads a column that
    /// is not there is a detector reading ground truth, and the column is not there for
    /// exactly that reason.
    fn on_messages(&self, ctx: DetectorCtx, batch: &RecordBatch) -> Vec<Observation>;
}

/// A detector written in Python, wearing the trait above.
pub struct PyDetector {
    object: Py<PyAny>,
    card: ModelCard,
    last_error: Mutex<Option<String>>,
}

impl std::fmt::Debug for PyDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyDetector")
            .field("id", &self.card.id)
            .finish_non_exhaustive()
    }
}

impl PyDetector {
    /// Adapts a Python object, validating its card first.
    ///
    /// # Errors
    /// As [`PyCarFollowing::attach`], for the detector family.
    pub fn attach(object: Bound<'_, PyAny>) -> PyResult<Self> {
        let card_obj = object.getattr("card").map_err(|_| {
            err::V2xwError::new_err(
                "a plug-in must carry a model card: set a `card` attribute (03-interfaces.md §12)",
            )
        })?;
        let card = card_from_py(&card_obj)?;
        card.validate().map_err(|e| {
            err::V2xwError::new_err(format!("model card {}: {}", card.id, err::chain(&e)))
        })?;
        if card.family != Family::Detector {
            return Err(err::V2xwError::new_err(format!(
                "model card {} declares family {}, but it is being attached as a detector",
                card.id, card.family
            )));
        }
        if !object.getattr("on_messages").is_ok_and(|a| a.is_callable()) {
            return Err(err::V2xwError::new_err(format!(
                "detector plug-in {} has no callable `on_messages(ctx, batch)`",
                card.id
            )));
        }
        Ok(Self {
            object: object.unbind(),
            card,
            last_error: Mutex::new(None),
        })
    }

    /// The last traceback a call raised, if any.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }
}

impl Model for PyDetector {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Detector for PyDetector {
    fn on_messages(&self, ctx: DetectorCtx, batch: &RecordBatch) -> Vec<Observation> {
        Python::with_gil(|py| {
            // The batch crosses as Arrow, not as rows: `into_pyarrow` hands Python the same
            // buffers through the C data interface. A ten-thousand-message window costs one
            // call and no per-row allocation, which is the whole reason §15 batches.
            let result = batch
                .clone()
                .into_pyarrow(py)
                .and_then(|b| self.object.bind(py).call_method1("on_messages", (ctx, b)))
                .and_then(|r| r.extract::<Vec<Observation>>());
            match result {
                Ok(v) => v,
                Err(e) => {
                    if let Ok(mut slot) = self.last_error.lock() {
                        *slot = Some(e.to_string());
                    }
                    // A detector that raised reported nothing. Unlike an acceleration, an
                    // empty observation list is a *correct* value for "this window looked
                    // fine", so the error must not be silent: it is kept, and the
                    // conformance kit fails a plug-in whose `last_error` is set.
                    Vec::new()
                }
            }
        })
    }
}

/// A Python detector, as a handle Python can hold and the conformance kit can drive.
#[pyclass(name = "DetectorModel", module = "v2xw.plugins", frozen)]
pub struct PyDetectorHandle {
    inner: PyDetector,
}

#[pymethods]
impl PyDetectorHandle {
    /// Attaches a Python object as a detector.
    #[new]
    fn new(object: Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            inner: PyDetector::attach(object)?,
        })
    }

    /// The model's id.
    #[getter]
    fn id(&self) -> &str {
        &self.inner.card.id
    }

    /// The validated card.
    #[getter]
    fn card<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        crate::scenario::to_py_dict(py, "model card", &self.inner.card)
    }

    /// The last traceback a call raised, if any.
    #[getter]
    fn last_error(&self) -> Option<String> {
        self.inner.last_error()
    }

    /// Calls the detector through the **Rust trait object**, with a `pyarrow.RecordBatch`.
    ///
    /// `batch` is anything `pyarrow` will accept as a record batch. It is converted into
    /// Arrow's Rust representation, handed to `dyn Detector`, and handed back to Python as
    /// Arrow again — so the round trip the engine performs is the round trip tested.
    fn on_messages(
        &self,
        py: Python<'_>,
        t_ns: u64,
        node: u32,
        batch: &Bound<'_, PyAny>,
    ) -> PyResult<Vec<Observation>> {
        use arrow::pyarrow::FromPyArrow;
        let rb = RecordBatch::from_pyarrow_bound(batch)?;
        let model: &dyn Detector = &self.inner;
        let out = model.on_messages(DetectorCtx { t_ns, node }, &rb);
        if let Some(e) = self.inner.last_error() {
            return Err(err::V2xwError::new_err(format!(
                "detector {} raised: {e}",
                self.inner.card.id
            )));
        }
        let _ = py;
        Ok(out)
    }
}

/// The instant type the engine uses, re-exported so a signature reads the same on both
/// sides.
pub type Instant = SimTime;

/// The node id type, re-exported for the same reason.
pub type Node = NodeId;

/// Builds the `v2xw.plugins` submodule.
///
/// # Errors
/// Whatever `PyModule::add_class` returns.
pub fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    let m = PyModule::new(py, "plugins")?;
    m.add(
        "__doc__",
        "The Python plug-in seam: views, model cards and the family adapters.",
    )?;
    m.add_class::<PyEgo>()?;
    m.add_class::<PyLeader>()?;
    m.add_class::<PyLane>()?;
    m.add_class::<PyWeather>()?;
    m.add_class::<PyCarFollowingHandle>()?;
    m.add_class::<PyDetectorHandle>()?;
    m.add_class::<DetectorCtx>()?;
    m.add_class::<PyReferenceDetector>()?;
    m.add_class::<Observation>()?;
    m.add_function(wrap_pyfunction!(validate_card, &m)?)?;
    m.add_function(wrap_pyfunction!(uncited_parameters, &m)?)?;
    m.add("PROBABILITY_QUANTUM", PROBABILITY_QUANTUM)?;
    let _ = PyDict::new(py);
    Ok(m)
}

impl PyCarFollowingHandle {
    /// The adapter behind the handle, for the conformance kit.
    pub fn model(&self) -> &PyCarFollowing {
        &self.inner
    }
}

impl PyDetectorHandle {
    /// The adapter behind the handle, for the conformance kit.
    pub fn model(&self) -> &PyDetector {
        &self.inner
    }
}

/// The Rust reference detector, so a Python detector can be compared against one.
#[pyclass(name = "ReferenceDetector", module = "v2xw.plugins", frozen)]
pub struct PyReferenceDetector {
    inner: SpeedPlausibilityDetector,
}

#[pymethods]
impl PyReferenceDetector {
    /// The reference detector, optionally with a different threshold.
    #[new]
    #[pyo3(signature = (max_speed_mps=SpeedPlausibilityDetector::DEFAULT_MAX_SPEED_MPS))]
    fn new(max_speed_mps: f64) -> Self {
        Self {
            inner: SpeedPlausibilityDetector::new(max_speed_mps),
        }
    }

    /// Its card.
    #[getter]
    fn card<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        crate::scenario::to_py_dict(py, "model card", self.inner.card())
    }

    /// Runs the detector over a `pyarrow.RecordBatch`, through `dyn Detector`.
    fn on_messages(
        &self,
        t_ns: u64,
        node: u32,
        batch: &Bound<'_, PyAny>,
    ) -> PyResult<Vec<Observation>> {
        use arrow::pyarrow::FromPyArrow;
        let rb = RecordBatch::from_pyarrow_bound(batch)?;
        let model: &dyn Detector = &self.inner;
        Ok(model.on_messages(DetectorCtx { t_ns, node }, &rb))
    }
}

/// A grid of car-following scenes, as `(ego speed, gap, leader speed)`.
///
/// The conformance kit calls a model at every point of this grid. The points are chosen to
/// cover the cases a longitudinal model gets wrong: standstill, a closing gap, a free road
/// (`gap = None`), and a leader faster than the ego.
pub fn conformance_grid() -> Vec<(f64, Option<f64>, f64)> {
    let speeds = [0.0, 5.0, 13.9, 27.8];
    let gaps = [None, Some(2.0), Some(10.0), Some(50.0), Some(200.0)];
    let leader_speeds = [0.0, 8.0, 30.0];
    let mut out = Vec::with_capacity(speeds.len() * gaps.len() * leader_speeds.len());
    for v in speeds {
        for g in gaps {
            for lv in leader_speeds {
                out.push((v, g, lv));
            }
        }
    }
    out
}

/// Calls `model` at every point of [`conformance_grid`] through `dyn CarFollowing`.
///
/// Through the trait object on purpose: the property being tested is that *the engine's*
/// call produces the same answer twice, not that a Python method does.
pub fn sweep(model: &dyn CarFollowing) -> Vec<f64> {
    conformance_grid()
        .into_iter()
        .map(|(v, gap, lv)| {
            let (mut ego, lane, weather) = synthetic_scene(v, URBAN_SPEED_LIMIT_MPS);
            ego.speed_mps = v;
            let leader = gap.map(|g| {
                let mut l = ego;
                l.speed_mps = lv;
                LeaderView::of(l, g)
            });
            model.accel(&ego, leader.as_ref(), &lane, &weather)
        })
        .collect()
}

/// A reference detector in Rust, so the [`Detector`] trait is exercised without Python.
///
/// It implements one check from 07-threats-and-detection.md's local-plausibility set: a
/// claimed speed above what any road vehicle reaches is not plausible. That is the whole
/// model — it exists to prove the seam, and its card says so.
///
/// Its purpose here is comparison. A Python detector and this one are handed the same
/// record batch through the same trait object, and a conformance suite that expects them to
/// agree on an obvious case is checking the adapter rather than the detector.
#[derive(Debug)]
pub struct SpeedPlausibilityDetector {
    card: ModelCard,
    /// The speed above which a claim is implausible, m/s.
    max_speed_mps: f64,
}

impl Default for SpeedPlausibilityDetector {
    fn default() -> Self {
        Self::new(SpeedPlausibilityDetector::DEFAULT_MAX_SPEED_MPS)
    }
}

impl SpeedPlausibilityDetector {
    /// The default threshold, m/s.
    ///
    /// Uncalibrated, and marked as such on the card: 90 m/s is roughly 324 km/h, chosen as
    /// "faster than any road vehicle" and not fitted to anything. The calibration plan on
    /// the card says what it would take to earn a number here.
    pub const DEFAULT_MAX_SPEED_MPS: f64 = 90.0;

    /// A detector with the given threshold.
    pub fn new(max_speed_mps: f64) -> Self {
        let mut card = ModelCard::new(
            "detect/local/speed-plausibility",
            Family::Detector,
            "0.1.0",
            "Flags a claimed speed above a fixed threshold as implausible. A reference \
             implementation for the detector seam, not a research detector.",
        );
        card.parameters = vec![v2xw_core::card::Parameter::new(
            "max_speed_mps",
            "m/s",
            serde_json::json!(Self::DEFAULT_MAX_SPEED_MPS),
            v2xw_core::card::Source::todo_calibrate(
                "no published threshold: 90 m/s is 'faster than any road vehicle', not a \
                 fitted value",
            ),
        )];
        // Registry rule R1: a `todo-calibrate` default must carry the plan for calibrating
        // it. Without this the card does not validate, which is the rule working.
        card.parameters[0].calibration = Some(
            "Take the speed distribution of the benign vehicles in a run of the target \
             scenario and set the threshold at the highest percentile that keeps the \
             false-accusation rate acceptable on the detection metrics (08-measurement §2.4). \
             Until that is done this detector's precision measures nothing."
                .to_string(),
        );
        card.determinism = v2xw_core::card::Determinism::default();
        card.limitations = vec![
            "A single global threshold. It cannot separate a spoofed position from a fast \
             vehicle, and it never fires on a plausible lie, which is most of them."
                .to_string(),
        ];
        Self {
            card,
            max_speed_mps,
        }
    }
}

impl Model for SpeedPlausibilityDetector {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Detector for SpeedPlausibilityDetector {
    fn on_messages(&self, ctx: DetectorCtx, batch: &RecordBatch) -> Vec<Observation> {
        use arrow::array::{Array, Float64Array, UInt32Array};
        let Some(speeds) = batch
            .column_by_name("speed_mps")
            .and_then(|c| c.as_any().downcast_ref::<Float64Array>())
        else {
            // A column that is not there is not a zero. Reporting nothing is the honest
            // answer for a batch this detector cannot read, and the caller's
            // `last_error`-shaped question is answered by the empty result plus the fact
            // that the column is absent from the schema.
            return Vec::new();
        };
        let subjects = batch
            .column_by_name("node")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let mut out = Vec::new();
        for i in 0..speeds.len() {
            if speeds.is_null(i) {
                continue;
            }
            let v = speeds.value(i);
            if v > self.max_speed_mps {
                out.push(Observation {
                    subject: subjects.map_or(0, |s| s.value(i)),
                    kind: "speed-plausibility".to_string(),
                    confidence: v2xw_core::math::quantize_to(1.0, PROBABILITY_QUANTUM),
                    t_ns: ctx.t_ns,
                });
            }
        }
        out
    }
}
