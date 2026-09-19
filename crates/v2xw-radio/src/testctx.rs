//! A minimal [`Ctx`] implementation, for this crate's own tests.
//!
//! Compiled only under `cfg(test)`. Every model in this crate is written against
//! `C: Ctx` with no further bound, so the tests can drive them with this rather than with
//! an engine — which is the point: the EDCA state machine, the AR(1) shadowing process
//! and the PER model are all testable without a kernel, a world or a scheduler.
//!
//! The world is a real [`v2xw_world::model::World`], built by [`tiny_world`], because the
//! obstacle models take one.

use v2xw_core::ctx::{Ctx, ErasedRecord, Visibility};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::geom::Vec3;
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::SimTime;
use v2xw_world::ImportOptions;
use v2xw_world::model::{Building, HeightSource, MaterialClass, World};

/// The event payload a test context carries: a label, which is all any test needs.
pub type TestPayload = &'static str;

/// A context with a clock a test can set, a seeded RNG registry, a world and a record
/// sink.
pub struct TestCtx {
    scheduler: Scheduler<TestPayload>,
    now: SimTime,
    rng: RngRegistry,
    provenance: ProvenanceLog,
    params: ParamSet,
    world: World,
    actors: Vec<v2xw_core::ids::ActorId>,
    /// Every record emitted, as `(channel, visibility, json)`.
    pub emitted: Vec<(&'static str, Visibility, String)>,
}

impl TestCtx {
    /// A context seeded with `seed`, holding [`tiny_world`].
    pub fn new(seed: u64) -> Self {
        Self {
            scheduler: Scheduler::new(),
            now: 0,
            rng: RngRegistry::new(seed),
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            world: tiny_world(),
            actors: Vec::new(),
            emitted: Vec::new(),
        }
    }

    /// A context holding a caller-supplied world.
    pub fn with_world(seed: u64, world: World) -> Self {
        let mut c = Self::new(seed);
        c.world = world;
        c
    }

    /// Moves the clock to `t`. The scheduler's own clock only advances when events are
    /// popped, and most tests here never schedule one.
    pub fn set_now(&mut self, t: SimTime) {
        self.now = t;
    }

    /// Advances the clock by `dt` nanoseconds.
    pub fn advance(&mut self, dt: u64) {
        self.now += dt;
    }
}

impl Ctx for TestCtx {
    type World = World;
    type Actors = Vec<v2xw_core::ids::ActorId>;
    type Payload = TestPayload;

    fn now(&self) -> SimTime {
        self.now
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.rng.checkout(domain, entity)
    }

    fn schedule(&mut self, at: SimTime, class: EventClass, payload: Self::Payload) -> EventHandle {
        self.scheduler.schedule(at, class, payload)
    }

    fn cancel(&mut self, handle: EventHandle) -> bool {
        self.scheduler.cancel(handle)
    }

    fn world(&self) -> &Self::World {
        &self.world
    }

    fn actors(&self) -> &Self::Actors {
        &self.actors
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        let mut bytes = Vec::new();
        record.write_json(&mut bytes).expect("record serialises");
        self.emitted.push((
            record.channel(),
            record.visibility(),
            String::from_utf8(bytes).expect("json is utf-8"),
        ));
    }

    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) {
        self.provenance.record(subject, model, params);
    }

    fn params(&self) -> &ParamSet {
        &self.params
    }
}

/// A small procedurally generated world: a 2 x 2 TR 36.885 urban grid.
///
/// The propagation and PHY tests never look at its geometry — they place endpoints by
/// hand — but [`v2xw_world::model::World`] requires a road network and a provenance
/// record, so the cheapest real world is the right stand-in for an empty one.
pub fn tiny_world() -> World {
    v2xw_world::procedural::grid(
        &v2xw_world::procedural::GridParams::tr36885_urban().with_size(2, 2),
        &ImportOptions::default().imported_at("1970-01-01T00:00:00Z"),
    )
    .expect("a 2 x 2 grid builds")
}

/// A world with one rectangular building, `[x0, x1] × [y0, y1]`, of `height_m`.
///
/// The obstacle tests place links across it and count the walls they cross.
pub fn world_with_building(x0: f64, y0: f64, x1: f64, y1: f64, height_m: f64) -> World {
    let footprint = vec![
        Vec3::new(x0, y0, 0.0),
        Vec3::new(x1, y0, 0.0),
        Vec3::new(x1, y1, 0.0),
        Vec3::new(x0, y1, 0.0),
    ];
    let mut world = tiny_world();
    let id = v2xw_core::ids::BuildingId::new(world.buildings.len() as u32);
    let building = Building::new(
        id,
        footprint,
        Vec::<Vec<Vec3>>::new(),
        height_m,
        0.0,
        MaterialClass::Unknown,
        HeightSource::Tagged,
    )
    .expect("a rectangle is a valid footprint");
    // `World`'s data fields are public and invariant I-W1 permits the loader to fill
    // them; the index cache is dropped so the next query rebuilds it.
    world.buildings.push(building);
    world.reindex();
    world
}
