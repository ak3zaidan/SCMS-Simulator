//! Regression pins for defects found in an adversarial review of `v2xw-core`.
//!
//! Each test below failed on the implementation as reviewed. They are kept as an
//! integration test — using only the crate's public API, the way a downstream crate sees
//! it — because that is the surface the defects were reachable from. The module-level
//! tests next to each fix cover the mechanism; these cover the contract.
//!
//! Delete a test only together with its defect.

use v2xw_core::ids::{ActorId, LaneId, NodeId};
use v2xw_core::math::{grid_index, is_on_grid, quantile_sorted, sort_total_order};
use v2xw_core::registry::ParamSet;
use v2xw_core::time::Duration;
use v2xw_core::weather::DrivingEffects;
use v2xw_core::{
    Bbox, ChannelName, Dims, EntityRef, ErasedRecord, EventClass, EventRecord, GeoOrigin, GridCell,
    GridIndex, Kinematics, LanePos, ModelCard, OwnedRecord, Parameter, PositionEstimate, Record,
    RegistryError, RngDomain, RngRegistry, Scheduler, Source, SourceKind, Vec3, Visibility,
    WeatherState,
};

/// `Bbox::union` is documented as "grows the box in place so that it contains `other`",
/// and `Bbox::empty()` is documented as "a box containing no points". Unioning a finite
/// box with an empty one must therefore leave the finite box unchanged.
///
/// It did not: `union` forwarded to `include(other.min)` / `include(other.max)`, and the
/// empty box's corners are `+INFINITY` / `-INFINITY`, so `min` collapsed to `-inf` and
/// `max` to `+inf` — the union of any box with an empty box was the whole of space.
///
/// This is on the path every accumulating bbox takes (`world_bbox = union of tile
/// bboxes`, focus-region unions, actor-extent unions): one empty contributor silently
/// turned the result into an infinite box that `is_empty()` reported as non-empty and
/// `contains()` reported as containing every point, which then poisoned any grid sizing or
/// spatial index derived from it.
#[test]
fn bbox_union_with_an_empty_box_must_be_a_no_op() {
    let finite = Bbox::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 10.0, 5.0));

    let mut b = finite;
    b.union(Bbox::empty());
    assert_eq!(b, finite, "union with an empty box must not change the box");

    // And the symmetric case: empty ∪ finite must be the finite box.
    let mut e = Bbox::empty();
    e.union(finite);
    assert_eq!(
        e, finite,
        "an empty box unioned with a finite box is that box"
    );
}

/// The sequence of `EventKey`s coming out of `Scheduler::pop` must be non-decreasing, or
/// the two guarantees the priority table exists to provide — `Control` "must be visible to
/// everything else at the same instant" and `Observe` "observes a fully settled instant"
/// (02-architecture.md §5.1) — are void, and every consumer that batches the dispatch
/// stream in key order is silently wrong.
///
/// `schedule` asserted only `at >= now`, so a handler running at instant *t* could inject
/// an event at *t* with a lower priority number and have it dispatched next.
#[test]
#[should_panic(expected = "earlier priority than the instant being dispatched")]
fn zero_delay_back_priority_scheduling_must_be_caught() {
    let mut s: Scheduler<&str> = Scheduler::new();
    s.schedule(1_000_000_000, EventClass::PhyEnd, "phy-end");
    let (k, _) = s.pop().unwrap();
    s.schedule(k.time, EventClass::Control, "control-after-the-fact");
}

/// Cancellation is lazy, and the heap was compacted only when a stale entry reached the
/// head — so a cancelled far-future entry was never reclaimed. The engine's timers are
/// cancel-heavy (MAC backoff abandoned when the channel goes busy, SPS reservations,
/// protocol timeouts rescheduled each tick) and §5.4 wants 24-hour backend runs, so the
/// dead entries would accumulate for the whole run while `len()` reported them as absent.
#[test]
fn cancelled_far_future_events_must_not_pin_the_heap() {
    let mut s: Scheduler<u32> = Scheduler::new();
    let handles: Vec<_> = (0..100_000u32)
        .map(|i| s.schedule(1_000_000_000 + i as u64, EventClass::MacTimer, i))
        .collect();
    for h in handles.iter().take(99_000) {
        s.cancel(*h);
    }
    assert_eq!(s.len(), 1_000);
    assert!(
        s.pending_entries() < 4_000,
        "{} heap entries for {} live events",
        s.pending_entries(),
        s.len()
    );
    // …and the surviving events still come out in exactly the right order.
    let popped: Vec<u32> = std::iter::from_fn(|| s.pop().map(|(_, p)| p)).collect();
    assert_eq!(popped, (99_000..100_000).collect::<Vec<_>>());
}

/// A backwards `extrapolate` used to clamp the elapsed time to zero but still stamp the
/// requested, earlier `t` on the result — a state claiming to be valid at `t` while
/// carrying a position from after `t`. Re-extrapolating it forward, which the doc comment
/// itself warns consumers do, then double-counted the whole interval: 6.5 m at 13 m/s over
/// 500 ms, silently.
#[test]
fn backward_extrapolation_must_keep_the_states_own_timestamp() {
    let k = Kinematics {
        t: 1_000_000_000,
        pos: Vec3::new(10.0, 0.0, 0.0),
        vel: Vec3::new(13.0, 0.0, 0.0),
        acc: Vec3::ZERO,
        heading_rad: 0.0,
        yaw_rate_rad_s: 0.0,
        lane: None,
        dims: Dims::CAR,
    };
    let back = k.extrapolate(k.t - 500_000_000);
    assert_eq!(back.t, k.t, "the result must carry its own validity time");
    assert_eq!(back, k);
    assert_eq!(
        back.extrapolate(k.t).pos.x,
        10.0,
        "re-extrapolating must not double-count the clamped interval"
    );
}

/// `API_VERSION` is documented as "a card declares the API version it targets; the
/// registry compares major versions", but nothing compared anything: a card claiming
/// `9.9.9` registered successfully.
#[test]
fn a_card_for_another_api_version_must_not_register() {
    let mut r = v2xw_core::Registry::new();
    let mut card = ModelCard::new(
        "radio/from-the-future",
        v2xw_core::Family::Propagation,
        "1.0.0",
        "a plug-in built against an API this engine does not implement",
    );
    card.api_version = "9.9.9".to_string();
    assert!(matches!(
        r.register(card),
        Err(RegistryError::InvalidCard(
            v2xw_core::CardError::ApiVersionMismatch { .. }
        ))
    ));
    assert!(r.is_empty());
}

/// The headline determinism claim is "identical outputs, single- or multi-threaded", and
/// the mandated parallelism is phase-parallel maps over actors, receivers and nodes
/// (02-architecture.md §6.4). The registry's only accessor took `&mut self`, so it could
/// not be called from inside such a map at all; the one escape hatch — `master_seed()`
/// plus `RngStream::derive` — restarts the stream at word 0, so a crate that used the
/// cached path single-threaded and the derive path when `--threads > 1` would have
/// produced different numbers with nothing detecting it.
#[test]
fn a_phase_parallel_map_must_draw_the_same_values_as_the_event_loop() {
    let parallel: Vec<u64> = {
        let reg = RngRegistry::new(7);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8u32)
                .map(|t| {
                    let reg = &reg;
                    scope.spawn(move || {
                        (t..256)
                            .step_by(8)
                            .map(|i| {
                                let e = EntityRef::Actor(ActorId::new(i));
                                (i, reg.checkout(RngDomain::Mobility, e).u64())
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            // Merged in id order, as §6.4 requires of every parallel phase.
            let mut merged: Vec<(u32, u64)> = handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect();
            merged.sort_by_key(|(i, _)| *i);
            merged.into_iter().map(|(_, v)| v).collect()
        })
    };

    let sequential: Vec<u64> = {
        let mut reg = RngRegistry::new(7);
        (0..256)
            .map(|i| {
                reg.stream(RngDomain::Mobility, EntityRef::Actor(ActorId::new(i)))
                    .u64()
            })
            .collect()
    };

    assert_eq!(
        parallel, sequential,
        "thread count changed the drawn values"
    );
}

/// Per-frame fading keys must not be interned: one cached generator per frame per directed
/// link is ~3 × 10⁸ streams at the design target, tens of gigabytes, with `clear()` and
/// `forget()` the only reclamation and both of them rewinding surviving streams.
#[test]
fn per_frame_streams_must_not_accumulate() {
    let reg = RngRegistry::new(1);
    let link = v2xw_core::LinkKey::new(NodeId::new(1), NodeId::new(2));
    for frame in 0..10_000 {
        let _ = reg
            .checkout(RngDomain::Fading, EntityRef::LinkFrame { link, frame })
            .f64();
    }
    assert!(reg.is_empty(), "interned {} per-frame streams", reg.len());
}

/// Model-card canonical bytes must be sorted at every level, not merely "whatever
/// `serde_json::Value`'s map happens to be": `serde_json/preserve_order` is a global,
/// feature-unified decision any future dependency of any workspace crate can make, and it
/// would otherwise change every registry content hash and every manifest that pins one.
#[test]
fn canonical_card_bytes_must_be_key_sorted() {
    let card = ModelCard::new(
        "radio/a",
        v2xw_core::Family::Propagation,
        "1.0.0",
        "a model",
    );
    let bytes = String::from_utf8(card.canonical_bytes().unwrap()).unwrap();
    assert!(bytes.starts_with(r#"{"api_version":"#), "{bytes}");
    let keys: Vec<&str> = bytes
        .split(",\"")
        .filter_map(|s| s.split("\":").next())
        .map(|s| s.trim_start_matches(['{', '"']))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "top-level keys are not sorted: {keys:?}");
}

// ---------------------------------------------------------------------------------------
// Defects found by an adversarial re-review of the completion pass.
// ---------------------------------------------------------------------------------------

/// Three contract types used `f64::INFINITY` as an in-band sentinel and derived both
/// `Serialize` and `Deserialize`, so their *neutral* constants serialised to JSON `null`
/// and then failed to deserialise:
///
/// | Type | Constructor | Infinite field(s) |
/// |---|---|---|
/// | `PositionEstimate` | `no_fix(t)` | `semi_major_m`, `semi_minor_m` |
/// | `WeatherState` | `CLEAR` (also `Default`) | `visibility_m` |
/// | `DrivingEffects` | `UNAFFECTED` (also `Default`) | `max_decel_mps2`, `visibility_m` |
///
/// These are the values that dominate a run — the weather of every scenario that says
/// nothing about weather, the effects a weather model must return for it, and the belief
/// every node holds before its first fix, in a tunnel or under jamming — so a run could
/// record a clear-weather keyframe and then fail to replay its own recording. The unit
/// tests missed it because each round-tripped a *finite* instance and probed the constant
/// with `is_infinite()`. (Previously pinned as the expected-to-fail
/// `open_defect_serde_sentinels.rs`.)
#[test]
fn infinite_sentinels_must_survive_a_json_round_trip() {
    let mut broken: Vec<String> = Vec::new();

    let pe = PositionEstimate::no_fix(7);
    let json = serde_json::to_string(&pe).expect("serialising a no-fix estimate");
    assert!(
        json.contains("\"semi_major_m\":null"),
        "the sentinel must travel as null: {json}"
    );
    match serde_json::from_str::<PositionEstimate>(&json) {
        Ok(back) => assert_eq!(back, pe),
        Err(e) => broken.push(format!("PositionEstimate::no_fix -> {json} -> {e}")),
    }

    let w = WeatherState::CLEAR;
    let json = serde_json::to_string(&w).expect("serialising CLEAR");
    match serde_json::from_str::<WeatherState>(&json) {
        Ok(back) => assert_eq!(back, w),
        Err(e) => broken.push(format!("WeatherState::CLEAR -> {json} -> {e}")),
    }
    // …and `CLEAR` is also `Default`, so this is the shape a scenario that mentions no
    // weather at all writes.
    assert_eq!(WeatherState::default(), WeatherState::CLEAR);

    let d = DrivingEffects::UNAFFECTED;
    let json = serde_json::to_string(&d).expect("serialising UNAFFECTED");
    match serde_json::from_str::<DrivingEffects>(&json) {
        Ok(back) => assert_eq!(back, d),
        Err(e) => broken.push(format!("DrivingEffects::UNAFFECTED -> {json} -> {e}")),
    }
    assert_eq!(DrivingEffects::default(), DrivingEffects::UNAFFECTED);

    assert!(
        broken.is_empty(),
        "a value this crate publishes as a constant cannot be read back from its own \
         serialisation:\n  {}",
        broken.join("\n  ")
    );
}

/// `GeoOrigin` reaches recorded, exported and digested artefacts (`World::origin`, the
/// world provenance, the manifest, the UI `Hello`) and was the only such core type with no
/// `Q_*` constants and no `quantized()` — against 03-interfaces.md §1's rule, and already
/// worked around downstream, where `v2xw-world/src/quant.rs` declares `Q_DEGREES = 1e-7`
/// itself with the comment "D9's table has no entry for degrees, so this one is declared
/// here". Degrees cross world → msg → server, so the quantum was about to be declared a
/// third time.
#[test]
fn the_geodetic_origin_must_quantise_like_every_other_exported_type() {
    assert_eq!(
        GeoOrigin::Q_DEG,
        1e-7,
        "the quantum v2xw-world had to invent"
    );
    assert_eq!(GeoOrigin::Q_ALT_M, 1e-3);

    let o = GeoOrigin::new(40.744_000_049_9, -73.990_000_06, 12.500_499_9);
    let q = o.quantized();
    assert!(is_on_grid(q.lat_deg, GeoOrigin::Q_DEG));
    assert!(is_on_grid(q.lon_deg, GeoOrigin::Q_DEG));
    assert!(is_on_grid(q.alt_m, GeoOrigin::Q_ALT_M));
    assert_eq!(q.quantized(), q);
    assert!(
        !is_on_grid(o.lat_deg, GeoOrigin::Q_DEG),
        "not a vacuous test"
    );

    // A stable cross-platform digest needs the integer multiple, not the rounded float —
    // the helper `v2xw-world` had to invent as `crate::quant::grid_index`.
    assert_eq!(grid_index(q.lat_deg, GeoOrigin::Q_DEG), 407_440_000);
    assert_eq!(
        grid_index(40.744_000_000_000_01, GeoOrigin::Q_DEG),
        grid_index(40.743_999_999_999_99, GeoOrigin::Q_DEG)
    );
    assert_eq!(grid_index(f64::INFINITY, 1e-3), i64::MAX);
    assert_eq!(grid_index(f64::NAN, 1e-3), i64::MIN);
}

/// `Kinematics` is the payload of the `gt.kinematics` recording channel
/// (03-interfaces.md §14), of every UI keyframe and delta, and of the ground-truth labels
/// a dataset is built from — and it had no `quantized()` and no `Q_*` constants, while its
/// belief twin `PositionEstimate` carried both for the same fields. The *truth* channel
/// escaped the D9 writer-side quantiser while the *belief* channel did not, which is the
/// asymmetry ADR 0004's own evidence (one field that escaped `round(x, 3)`) was about.
#[test]
fn the_ground_truth_channel_must_quantise_like_its_belief_twin() {
    assert_eq!(Kinematics::Q_M, PositionEstimate::Q_M);
    assert_eq!(Kinematics::Q_RAD, PositionEstimate::Q_RAD);

    let k = Kinematics {
        t: 1_000_000_000,
        pos: Vec3::new(10.000_499_9, 20.000_6, -0.123_456_7),
        vel: Vec3::new(3.141_492_6, -4.000_04, 0.0),
        acc: Vec3::new(1.500_499_9, 0.0, 0.0),
        heading_rad: 0.750_000_499_9,
        yaw_rate_rad_s: -0.020_000_000_5,
        lane: Some(LanePos::new(LaneId::new(3), 42.000_499_9, 0.100_6)),
        dims: Dims::new(4.500_499_9, 1.800_06, 1.500_000_1),
    };
    let q = k.quantized();

    for x in [
        q.pos.x,
        q.pos.y,
        q.pos.z,
        q.vel.x,
        q.vel.y,
        q.vel.z,
        q.acc.x,
        q.acc.y,
        q.acc.z,
        q.lane.unwrap().s_m,
        q.lane.unwrap().d_m,
        q.dims.length_m,
        q.dims.width_m,
        q.dims.height_m,
    ] {
        assert!(is_on_grid(x, Kinematics::Q_M), "{x} is off the metre grid");
    }
    for a in [q.heading_rad, q.yaw_rate_rad_s] {
        assert!(
            is_on_grid(a, Kinematics::Q_RAD),
            "{a} is off the angle grid"
        );
    }
    assert!(!is_on_grid(k.pos.z, Kinematics::Q_M), "not a vacuous test");
    assert_eq!(q.quantized(), q, "quantisation must be idempotent");
    assert_eq!(q.t, k.t, "an integer needs no grid");

    // The components carry their own grid, for a writer that holds only one of them.
    assert_eq!(LanePos::Q_M, Kinematics::Q_M);
    assert_eq!(Dims::Q_M, Kinematics::Q_M);
    assert_eq!(Dims::CAR.quantized(), Dims::CAR);
    assert_eq!(
        LanePos::new(LaneId::new(1), 1.000_499_9, -0.000_6).quantized(),
        LanePos::new(LaneId::new(1), 1.0, -0.001)
    );
}

/// `ParamSet::resolve`'s range check was type-strict where its type check is deliberately
/// type-lenient, so it rejected a valid scenario. `json_type_name` calls integers and
/// floats one type on purpose ("JSON, YAML and every authoring format spell 2 and 2.0
/// interchangeably"), but the set-membership branch fell through to `Value` equality, and
/// `serde_json` distinguishes `Number::PosInt(1)` from `Number::Float(1.0)`: a card
/// declaring `range: [0.1, 0.5, 1.0]` accepted `{"alpha": 1.0}` and rejected
/// `{"alpha": 1}` — with a message that listed the value it had just refused among the
/// allowed ones. YAML authoring produces the integer spelling routinely.
#[test]
fn a_set_range_must_accept_either_spelling_of_the_same_number() {
    let mut card = ModelCard::new(
        "radio/spelling",
        v2xw_core::Family::Propagation,
        "1.0.0",
        "a model with an enumerated parameter",
    );
    let mut alpha = Parameter::new(
        "alpha",
        "-",
        serde_json::json!(0.5),
        Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §5"),
    );
    alpha.range = Some(vec![
        serde_json::json!(0.1),
        serde_json::json!(0.5),
        serde_json::json!(1.0),
    ]);
    card.parameters.push(alpha);

    for spelling in [serde_json::json!(1.0), serde_json::json!(1)] {
        let set = ParamSet::resolve(&card, &serde_json::json!({ "alpha": spelling }))
            .unwrap_or_else(|e| panic!("{spelling} must be accepted: {e}"));
        assert_eq!(set.get_f64("alpha"), Some(1.0));
    }
    // A number genuinely outside the set is still refused.
    assert!(ParamSet::resolve(&card, &serde_json::json!({"alpha": 2})).is_err());
}

/// A declared default was never checked against the parameter's own declared range — not
/// by `ModelCard::validate`, not by `Registry::register`, and not by `ParamSet::resolve`,
/// which range-checks overrides but copies defaults in verbatim. A card with
/// `default: 40.0, range: [0.0, 10.0]` validated, registered, and resolved to 40.0, so a
/// scenario that does not mention the parameter — the common case — ran the model on a
/// value the card itself declares impossible, while the check on overrides gave false
/// assurance that this could not happen.
#[test]
fn a_default_outside_its_own_declared_range_must_not_register() {
    let mut card = ModelCard::new(
        "radio/self-contradicting",
        v2xw_core::Family::Propagation,
        "1.0.0",
        "a model whose card contradicts itself",
    );
    let mut p = Parameter::new(
        "beta",
        "-",
        serde_json::json!(40.0),
        Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §5"),
    );
    p.range = Some(vec![serde_json::json!(0.0), serde_json::json!(10.0)]);
    card.parameters.push(p);

    assert!(matches!(
        card.validate(),
        Err(v2xw_core::CardError::DefaultOutOfRange { ref parameter, .. }) if parameter == "beta"
    ));

    // Registration refuses it, so the failure names the plug-in…
    let mut r = v2xw_core::Registry::new();
    assert!(matches!(
        r.register(card.clone()),
        Err(RegistryError::InvalidCard(
            v2xw_core::CardError::DefaultOutOfRange { .. }
        ))
    ));
    assert!(r.is_empty());

    // …rather than the scenario silently running on the impossible value.
    card.parameters[0].default = serde_json::json!(4.0);
    assert_eq!(card.validate(), Ok(()));
    let set = ParamSet::resolve(&card, &serde_json::Value::Null).unwrap();
    assert_eq!(set.get_f64("beta"), Some(4.0));
}

/// `Duration::checked_div(k)` was `Duration(self.0 / k)`: it panicked on `k == 0` instead
/// of returning an `Option`, contradicting the universal Rust meaning of the `checked_`
/// prefix. Every other `Duration` operation saturates as documented; this one was the
/// single panicking path, and being `const fn` the panic was a hard compile error in a
/// const context too. A model computing `period.checked_div(n_slots)` with an empty slot
/// set aborted the run.
#[test]
fn dividing_a_duration_by_zero_must_not_abort_the_run() {
    assert_eq!(
        Duration::from_millis(100).checked_div(4),
        Some(Duration::from_millis(25))
    );
    assert_eq!(
        Duration::from_millis(100).checked_div(0),
        None,
        "`checked_` means checked"
    );
    assert_eq!(
        Duration::from_millis(100).saturating_div(0),
        Duration::MAX,
        "the total spelling saturates instead of panicking"
    );
    // The shape that used to abort a run: an empty slot set.
    let n_slots: u64 = 0;
    let period = Duration::from_millis(100);
    assert_eq!(
        period.checked_div(n_slots).unwrap_or(Duration::ZERO),
        Duration::ZERO
    );
    // A const context, where the old body was a compile error rather than a panic.
    const SLOT: Option<Duration> = Duration::from_secs(1).checked_div(0);
    assert_eq!(SLOT, None);
}

/// `GridIndex::cells_within` silently returned **zero** cells for a very large or infinite
/// radius — the exact opposite of its own comment ("an enormous radius covers the whole
/// coordinate space, not a wrapped fragment of it"). The saturating branch set
/// `rings = u32::MAX` and `neighborhood_radius` then did `r as i32`, giving `-1`, so the
/// range `(-r..=r)` was `(1..=-1)`: empty. A model expressing "unlimited range" as an
/// infinite radius got an empty candidate set — a query that should match everything
/// matched nothing, with no diagnostic. A direct call with `r = 2^31` additionally
/// overflowed on `-r` (a panic in debug builds).
///
/// The set is checked by its bounds and by a bounded `take`, never counted: the whole
/// coordinate space is 2^62 cells, and it is yielded lazily.
#[test]
fn an_unbounded_range_query_must_not_match_nothing() {
    let g = GridIndex::new(100.0);
    let p = Vec3::new_2d(250.0, 250.0);
    let centre = g.cell_of(p);

    for radius in [f64::INFINITY, 1e40] {
        let mut it = g.cells_within(p, radius);
        let first = it.next().expect("an unbounded query must yield cells");
        assert_eq!(
            first,
            GridCell::new(
                centre.x.saturating_sub(i32::MAX),
                centre.y.saturating_sub(i32::MAX)
            ),
            "radius {radius} must span the coordinate space"
        );
        assert_eq!(g.cells_within(p, radius).take(1_000).count(), 1_000);
    }

    // A bounded radius is unchanged: ceil(radius / size) rings around the centre cell.
    assert_eq!(g.cells_within(p, 2_000.0).count(), 41 * 41);
    assert_eq!(g.cells_within(p, 0.0).collect::<Vec<_>>(), vec![centre]);

    // And a huge ring count no longer overflows on `-r`.
    assert_eq!(
        g.neighborhood_radius(GridCell::new(0, 0), 1u32 << 31)
            .next()
            .unwrap(),
        GridCell::new(-i32::MAX, -i32::MAX)
    );
}

/// `Ctx::emit_erased` hands a recorder a borrow that dies with the call, so a recorder
/// that batches had to either JSON-encode on the hot path or downcast every channel it
/// knows through `as_any` — which inverts the plug-in architecture. `to_owned_record` is
/// the owned form it can queue, and `ChannelName` is the recording channel as a type, so
/// §10's `subscribe()` cannot be confused with §4's radio `ChannelId`.
#[test]
fn a_recorder_that_batches_must_be_able_to_keep_a_record() {
    #[derive(serde::Serialize)]
    struct Cbr {
        node: NodeId,
        cbr: f64,
    }
    impl Record for Cbr {
        const CHANNEL: &'static str = "mac.cbr";
        const VISIBILITY: Visibility = Visibility::Node;
    }

    let mut queue: Vec<OwnedRecord> = Vec::new();
    let mut write = |r: &dyn ErasedRecord| queue.push(r.to_owned_record().unwrap());
    write(&Cbr {
        node: NodeId::new(7),
        cbr: 0.42,
    });

    let owned = &queue[0];
    assert_eq!(owned.channel, "mac.cbr");
    assert_eq!(owned.visibility, Visibility::Node);
    assert_eq!(owned.json_str().unwrap(), r#"{"node":7,"cbr":0.42}"#);
    assert_eq!(owned.channel_name(), ChannelName::of::<Cbr>());
    assert_eq!(ChannelName::of::<Cbr>().as_str(), Cbr::CHANNEL);

    // §10 names the owned form `EventRecord`.
    let ev: &EventRecord = owned;
    assert_eq!(ev.channel_name().to_string(), "mac.cbr");
}

/// A metric's p50/p95 needs one deterministic order over `f64` (which has no `Ord`) and
/// one documented interpolation rule, or two engines disagree on a recorded p95.
#[test]
fn quantiles_have_one_order_and_one_rule() {
    let base = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
    let mut reference = base;
    sort_total_order(&mut reference);
    assert_eq!(reference, [1.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 9.0]);

    // Arrival order cannot change the result — the property a phase-parallel reduction
    // needs, and the one `sum_sorted_by_key` already provides for sums.
    for rotation in 1..base.len() {
        let mut rotated = base;
        rotated.rotate_left(rotation);
        sort_total_order(&mut rotated);
        assert_eq!(rotated, reference, "rotation {rotation} changed the order");
        assert_eq!(
            quantile_sorted(&rotated, 0.95).to_bits(),
            quantile_sorted(&reference, 0.95).to_bits()
        );
    }

    assert_eq!(
        quantile_sorted(&reference, 0.5),
        3.5,
        "type-7 interpolation"
    );
    assert_eq!(quantile_sorted(&reference, 0.0), 1.0);
    assert_eq!(quantile_sorted(&reference, 1.0), 9.0);
    assert!(quantile_sorted(&[], 0.5).is_nan(), "the median of nothing");
}
