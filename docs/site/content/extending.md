A plug-in is a Rust type that carries a model card and implements one family trait.
There is no registration macro, no dynamic loading, and no configuration file: the
registry takes the card, validates it, hashes it, and the manifest pins the hash. This
page walks one family end to end — propagation, because it is small enough to show
whole and it exercises every rule the engine cares about.

The seams available are listed in the architecture page's plug-in section: in-process
Rust trait objects for hot paths, batched Python through PyO3 for control-plane
families, and out-of-process gRPC for external tools. Everything below is the first
kind.

## What you are going to write

| Piece | Why the engine needs it |
|---|---|
| A card-building function | A model with no card cannot be registered. The card is what the manifest pins and what this site's [model reference](models.html) is generated from |
| A struct holding that card | `Model::card` returns a borrow, and the card returned at run time must be the one registered — the registry hashed it |
| An `impl Model` | One method. Everything else on the trait is defaulted from the card |
| An `impl Propagation<C>` | The family trait: what the model actually computes |
| Registration | One call that hands the registry the model |
| Tests | Including at least one that has been shown to fail |

## Step 1 — the card

The card is not documentation attached to the model; it is the model's declared
interface. Every parameter the model reads at run time must appear here with a unit, a
default and a source, and the conformance tracer fails a model that reads an undeclared
one.

```rust
use serde_json::json;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier,
    Validation, ValidationStatus,
};

/// The stable id. Lower case, slash-separated, and never changed once published:
/// scenarios and manifests name the model by it.
pub const MODEL_ID: &str = "propagation/two-ray-flat";
pub const MODEL_VERSION: &str = "1.0.0";

fn card(params: &TwoRayParams) -> ModelCard {
    let rappaport = Source::new(
        SourceKind::Paper,
        "T. S. Rappaport, Wireless Communications: Principles and Practice, 2nd ed., \
         §4.6 (two-ray ground reflection)",
    );

    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Propagation,
        MODEL_VERSION,
        "Two-ray ground-reflection path loss over a flat earth: the interference of the \
         direct ray and the ground-reflected ray, which beyond the breakpoint decays as \
         the fourth power of distance rather than the second.",
    );
    card.tier = vec![Tier::Medium];

    card.equations = vec![
        Equation::new(
            "breakpoint",
            "d_c = 4 · h_tx · h_rx · f / c",
        ),
        Equation::new(
            "path loss",
            "L[dB] = 20·log10(4πd f / c)                     for d <= d_c\n\
             L[dB] = 40·log10(d) − 20·log10(h_tx · h_rx)     for d >  d_c",
        ),
    ];

    card.parameters = vec![
        Parameter::new(
            "antenna_height_tx_m",
            "m",
            json!(params.h_tx_m),
            rappaport.clone(),
        ),
        Parameter::new(
            "antenna_height_rx_m",
            "m",
            json!(params.h_rx_m),
            rappaport.clone(),
        ),
        // A value nobody has a citation for must say so, and must carry a plan.
        // The registry refuses the card otherwise (card rule R1), and the parameter
        // appears on this site's calibration-debt page until the plan is carried out.
        Parameter {
            calibration: Some(
                "Sweep 0.0–6.0 dB against the measured urban PDR-versus-distance curve \
                 of R3 §A.3 and pick the value minimising RMS error; until then this is \
                 a guess and any result sensitive to it is provisional."
                    .to_string()
            ),
            ..Parameter::new(
                "ground_loss_db",
                "dB",
                json!(params.ground_loss_db),
                Source::todo_calibrate("an allowance for imperfect ground reflection"),
            )
        },
    ];

    card.assumptions = vec![
        "The ground is flat and perfectly conducting between the two antennas.".to_string(),
        "Both antennas are above the ground plane by their stated heights.".to_string(),
    ];
    card.ignores = vec![
        "Fast fading — supply a `fading/*` model for that.".to_string(),
        "Buildings and vehicles — supply an `obstacle/*` model for that.".to_string(),
        "Terrain: the earth is flat here, so there is no diffraction.".to_string(),
    ];
    card.limitations = vec![
        "Below the breakpoint the two-ray form is not used, because the asymptotic \
         expression is wrong there; the free-space form is used instead, which makes the \
         loss curve continuous but not differentiable at d_c.".to_string(),
    ];

    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.determinism = Determinism::default(); // draws nothing; see step 3
    card.sources = vec![rappaport];
    card
}
```

Two things in that snippet matter more than the model:

- `Source::todo_calibrate` plus a `calibration` plan is how an uncited number is
  declared rather than hidden. `ModelCard::validate` rejects the card if the plan is
  missing, and the number then shows up on the [calibration debt](calibration.html)
  page with the plan beside it. **The engine has no way to express an uncited default
  quietly, and that is the point.**
- `card.ignores` is read by a human deciding whether this tier answers their question.
  Write it as the list of effects that are not in the answer, not as a list of features.

## Step 2 — the model

```rust
use v2xw_core::card::{ModelCard, Tier};
use v2xw_core::model::Model;

/// The model's own parameters, in the shape a scenario's `params` block deserialises.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoRayParams {
    pub h_tx_m: f64,
    pub h_rx_m: f64,
    pub ground_loss_db: f64,
}

impl Default for TwoRayParams {
    fn default() -> Self {
        Self { h_tx_m: 1.5, h_rx_m: 1.5, ground_loss_db: 0.0 }
    }
}

pub struct TwoRayFlat {
    card: ModelCard,
    params: TwoRayParams,
    tier: Tier,
}

impl TwoRayFlat {
    #[must_use]
    pub fn new(tier: Tier, params: TwoRayParams) -> Self {
        Self { card: card(&params), params, tier }
    }
}

impl Model for TwoRayFlat {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}
```

The card is built once, in the constructor, from the same `params` the model will
compute with. A model that built its card lazily, or built it from different values than
it uses, would make the manifest's pin a lie.

## Step 3 — the family trait

```rust
use v2xw_core::ctx::Ctx;
use v2xw_core::math;
use v2xw_core::weather::WeatherState;
use v2xw_radio::traits::Propagation;
use v2xw_radio::types::{LosResult, LossBreakdown, RadioEndpoint};

/// Speed of light, m/s. A constant, not a parameter: it is not calibrated.
const C_MPS: f64 = 299_792_458.0;

impl<C: Ctx + ?Sized> Propagation<C> for TwoRayFlat {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn loss_db(
        &mut self,
        _ctx: &mut C,
        tx: &RadioEndpoint,
        rx: &RadioEndpoint,
        f_hz: f64,
        _los: &LosResult,
        _w: &WeatherState,
    ) -> LossBreakdown {
        let d = tx.pos.distance(rx.pos).max(1.0);
        let h_tx = self.params.h_tx_m;
        let h_rx = self.params.h_rx_m;
        let d_c = 4.0 * h_tx * h_rx * f_hz / C_MPS;

        // `math::log10`, never `f64::log10`: the standard library routes to the
        // platform libm, whose precision varies by platform and by Rust version, and
        // two machines that disagree in the last bit of a path loss will eventually
        // disagree about whether a frame was received.
        let path_db = if d <= d_c {
            20.0 * math::log10(4.0 * core::f64::consts::PI * d * f_hz / C_MPS)
        } else {
            40.0 * math::log10(d) - 20.0 * math::log10(h_tx * h_rx)
        } + self.params.ground_loss_db;

        // `LossBreakdown::new` computes `total_db` from the terms with an ordered sum,
        // so the total can never disagree with its parts and two builds cannot disagree
        // about its last bit.
        LossBreakdown::new(path_db, 0.0, 0.0, 0.0, tx.gain_dbi + rx.gain_dbi)
    }
}
```

### If your model needs randomness

This one does not, and its card says so. A model that does must draw from a stream
keyed by `(domain, entity)` rather than from any generator it owns:

```rust
use v2xw_core::ids::{EntityRef, LinkKey};
use v2xw_core::rng::RngDomain;

let link = LinkKey(tx.node, rx.node);
let shadow_db = ctx.rng(RngDomain::Shadow, EntityRef::Link(link)).normal(0.0, sigma_db);
```

The key is the whole guarantee: one link's draw sequence does not depend on what any
other link drew, on the order the engine happened to evaluate links in, or on how many
threads it used. Declare it on the card:

```rust
card.determinism = Determinism {
    uses_rng: true,
    rng_domains: vec!["Shadow".to_string()],
};
```

A model that keeps a `rand::rngs::ThreadRng`, reads a wall clock, or holds per-link
state in a `HashMap` it then iterates to produce output has broken the reproducibility
contract in a way no test on one machine will show you.

## Step 4 — register it

```rust
use std::sync::Arc;
use v2xw_core::model::ModelHandle;
use v2xw_core::registry::Registry;

pub fn register(registry: &mut Registry) -> Result<(), v2xw_core::registry::RegistryError> {
    let model: ModelHandle = Arc::new(TwoRayFlat::new(Tier::Medium, TwoRayParams::default()));
    registry.register_model(model)?;
    Ok(())
}
```

`register_model` validates the card, hashes its canonical bytes, and stores the
registration. It refuses:

| Refusal | Cause |
|---|---|
| card validation error | an empty tier list, an empty purpose, a duplicate parameter name, a default outside its own declared range, or a `todo-calibrate` parameter with no plan |
| duplicate id | two different cards registered under one id. The same card twice is fine — two crates may legitimately publish the same model |
| licence gate | a copyleft licence declared for an in-process model. GPL and LGPL plug-ins run out of process only |

## Step 5 — select it from a scenario

```yaml
radio:
  rat: dsrc-80211p
  tiers:
    propagation: medium
    phy: medium
    mac: medium
  # `models` is keyed by family; the value is a registry id plus overrides.
  models:
    propagation:
      id: propagation/two-ray-flat
      params:
        antenna_height_tx_m: 1.5
        antenna_height_rx_m: 1.5
        ground_loss_db: 0.0
```

Anything the scenario does not name keeps the card's default, which is what makes a
parameter set content-addressable: the manifest records the set's hash, and two runs
that differ in one parameter have visibly different hashes.

> **What this build actually does with that block.** The `radio.models` field is in the
> schema, but the engine's wiring currently selects the propagation and fading models
> from the tier alone (`v2xw_engine::wiring::build_radio`) and does not read the map.
> Registering your model therefore puts it in the manifest and on this site, but does
> not yet make a scenario able to choose it. Reading `radio.models` in `build_radio` is
> owed work, not a design position — see the [defect register](defects.html) for how
> this project records gaps of that kind.

## Step 6 — the tests you owe

Write these before you believe the model.

```rust
#[test]
fn the_card_validates() {
    // The cheapest possible guard against a card that the registry would refuse at
    // load time, in the middle of someone else's run.
    card(&TwoRayParams::default()).validate().expect("card must validate");
}

#[test]
fn beyond_the_breakpoint_loss_grows_as_the_fourth_power() {
    // Doubling the distance must cost 12.04 dB in the two-ray regime, not 6.02 dB as
    // free space would. This is the assertion that distinguishes this model from the
    // one it is supposed to replace -- and therefore the assertion that can fail.
    let mut m = TwoRayFlat::new(Tier::Medium, TwoRayParams::default());
    let a = loss_at(&mut m, 400.0);
    let b = loss_at(&mut m, 800.0);
    assert!((b - a - 12.041).abs() < 1e-3, "got {} dB per doubling", b - a);
}
```

**Then make the test fail on purpose.** Change the exponent from 40.0 to 20.0 and
confirm the assertion goes red, and only then change it back. This project has a
recurring defect class: checks that cannot fail. Four of them were found in one review
pass, and the message layer being an unsigned stub survived a full vertical-slice audit
precisely because every check around it was satisfiable without the thing under test
working. A cooperative awareness message and a basic safety message both came out at
exactly 101 bytes; a single assertion that two different formats encode to different
sizes would have caught it on day one.

The [defect register](defects.html) collects the rest of those lessons.

## Other families

The same six steps apply to every seam. What changes is the trait and the context type
you are handed:

| Family | Trait | The context you get |
|---|---|---|
| Propagation, fading, obstacle, PHY, MAC, DCC | `v2xw_radio::traits::*` | the engine `Ctx`: clock, RNG streams, scheduling, world |
| Message generator, codec, envelope | `v2xw_msg::*`, `v2xw_sec::*` | the node's own view |
| Detector, attacker, misbehaviour pipeline | `v2xw_threat::*` | a `NodeView` — **never** the world |
| Metric provider, exporter | `v2xw_metrics::*`, `v2xw_record::*` | recorded records, not engine state |

The `NodeView` row is invariant I-C2 and the reason a detector written against this
engine is worth measuring: it cannot see the ground truth it is supposed to infer. If
you find yourself wanting the world inside a detector, the thing you want is a metric,
which runs outside the node and is allowed to know the answer.
