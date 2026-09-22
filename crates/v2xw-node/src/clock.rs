//! The node's own clock (06-node-models.md §2.3).
//!
//! A node does not know the time. It knows what its oscillator says, corrected by the last
//! GNSS fix it had. While a fix holds, the two agree; when the fix is lost the believed
//! time walks away from the true one at the oscillator's rate, and every freshness check
//! the node performs — a 1609.2 `generationTime` window, a CAM age, a replay guard — is
//! performed against the drifted value.
//!
//! That is the entire point of modelling it. A simulator where every node reads the
//! simulator's clock cannot produce a stale-message rejection, cannot produce a clock
//! attack, and reports a replay-detection rate that no fielded receiver would achieve.

use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

/// Model id of the drifting-oscillator clock.
pub const CLOCK_MODEL_ID: &str = "clock/gnss-locked-tcxo";

/// A node's clock: GNSS-locked when a fix exists, free-running otherwise.
#[derive(Debug, Clone)]
pub struct ClockModel {
    drift_ppm: f64,
    offset_ns: i64,
    locked: bool,
    last_update: SimTime,
    card: ModelCard,
}

impl ClockModel {
    /// A clock with the given free-running drift, in parts per million.
    ///
    /// Positive drift means the oscillator runs fast, so the believed time moves ahead of
    /// the true one while the fix is lost.
    pub fn new(drift_ppm: f64) -> Self {
        ClockModel {
            drift_ppm,
            offset_ns: 0,
            locked: true,
            last_update: 0,
            card: clock_card(drift_ppm),
        }
    }

    /// The free-running drift rate, ppm.
    pub fn drift_ppm(&self) -> f64 {
        self.drift_ppm
    }

    /// The current believed-minus-true offset, nanoseconds. **Ground truth**: the node
    /// cannot know it, and it reaches the telemetry record's one ground-truth integer
    /// field (`clock_offset_ns`, §3.5.2 at offset 40) precisely so that an analyst can see
    /// what the node could not.
    pub fn offset_ns(&self) -> i64 {
        self.offset_ns
    }

    /// Whether the clock is disciplined by a fix.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Advances the clock to `now`, accruing drift for the elapsed span when unlocked.
    ///
    /// Called once per node step, before anything reads [`ClockModel::believed_time`], so
    /// that everything within one step sees one consistent belief.
    pub fn advance(&mut self, now: SimTime, has_fix: bool) {
        let elapsed = now.saturating_sub(self.last_update);
        if !self.locked {
            // ppm is parts per million, so the accrued error over `elapsed` nanoseconds is
            // `elapsed * ppm / 1e6`. The multiplication is done in f64 and truncated once,
            // rather than accumulated per nanosecond, so the result depends only on the
            // total elapsed span and not on how the step was divided.
            let accrued = (elapsed as f64) * self.drift_ppm / 1e6;
            self.offset_ns = self.offset_ns.saturating_add(accrued as i64);
        }
        if has_fix && !self.locked {
            // Reacquiring a fix disciplines the oscillator: GNSS time is the reference and
            // the accumulated error is removed. Modelled as an instantaneous step rather
            // than a servo loop, which is the `medium` tier's simplification and is stated
            // on the card.
            self.offset_ns = 0;
        }
        self.locked = has_fix;
        self.last_update = now;
    }

    /// The instant this node believes it is.
    pub fn believed_time(&self, now: SimTime) -> SimTime {
        if self.offset_ns >= 0 {
            now.saturating_add(self.offset_ns.unsigned_abs())
        } else {
            now.saturating_sub(self.offset_ns.unsigned_abs())
        }
    }

    /// How far the belief is from the truth, as a span.
    pub fn error(&self, now: SimTime) -> Duration {
        Duration::between(
            now.min(self.believed_time(now)),
            now.max(self.believed_time(now)),
        )
    }

    /// Steps the clock by an offset, as an attacker with control of the time source would.
    pub fn step_offset(&mut self, by_ns: i64) {
        self.offset_ns = self.offset_ns.saturating_add(by_ns);
    }
}

impl Model for ClockModel {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

fn clock_card(drift_ppm: f64) -> ModelCard {
    let mut card = ModelCard::new(
        CLOCK_MODEL_ID,
        Family::Clock,
        "0.1.0",
        "A node's believed time: GNSS-disciplined while a fix holds, free-running at the \
         oscillator's drift rate otherwise (06-node-models.md §2.3).",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    let mut p = Parameter::new(
        "drift_ppm",
        "ppm",
        serde_json::json!(drift_ppm),
        Source::todo_calibrate("oscillator free-running drift rate"),
    );
    p.calibration = Some(
        "06-node-models §2.3 takes TCXO ppm values from 04-models §3.8, which records them \
         with sources or as TODO: calibrate. None of the OBU profiles of §7 publishes an \
         oscillator specification — not one of the ten names its TCXO — so no figure is \
         asserted here. Measure it by holding a device in a GNSS-denied chamber and \
         comparing its 1PPS against a disciplined reference over an hour, or take the \
         stability figure from the TCXO part number once the teardown identifies it."
            .to_string(),
    );
    card.parameters.push(p);
    card.equations.push(v2xw_core::card::Equation::new(
        "free-running offset",
        "offset(t + dt) = offset(t) + dt * drift_ppm / 1e6, while no fix is held",
    ));
    card.assumptions.push(
        "Reacquiring a fix removes the whole accumulated offset in one step; a real \
         receiver's disciplining loop would slew it away over seconds."
            .to_string(),
    );
    card.limitations.push(
        "One constant drift rate per node: no temperature dependence, no ageing, no \
         random walk. A holdover study needs the `high` tier and a measured Allan \
         deviation."
            .to_string(),
    );
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.sources.push(Source::new(
        SourceKind::TodoCalibrate,
        "06-node-models.md §2.3; 04-models.md §3.8",
    ));
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::time::NS_PER_S;

    /// While the fix holds, belief and truth agree; once it is lost they separate at the
    /// stated rate, and the separation is what a freshness check would see.
    #[test]
    fn belief_separates_from_truth_only_when_the_fix_is_lost() {
        let mut c = ClockModel::new(10.0);
        c.advance(NS_PER_S, true);
        assert_eq!(c.believed_time(NS_PER_S), NS_PER_S);
        assert_eq!(c.offset_ns(), 0);

        // Fix lost at t = 1 s. After 100 s of holdover at 10 ppm the clock is 1 ms fast.
        c.advance(NS_PER_S, false);
        c.advance(101 * NS_PER_S, false);
        assert_eq!(c.offset_ns(), 1_000_000);
        assert_eq!(c.believed_time(101 * NS_PER_S), 101 * NS_PER_S + 1_000_000);

        // Reacquisition disciplines it.
        c.advance(102 * NS_PER_S, true);
        assert_eq!(c.offset_ns(), 0);
    }

    /// Drift accrues on the total elapsed span, not per call: a step split in two gives
    /// the same offset as one step of the same length, which is what keeps a node's belief
    /// independent of how the engine happened to schedule it.
    #[test]
    fn drift_does_not_depend_on_how_the_step_was_divided() {
        let mut one = ClockModel::new(-20.0);
        one.advance(0, false);
        one.advance(60 * NS_PER_S, false);

        let mut many = ClockModel::new(-20.0);
        many.advance(0, false);
        for i in 1..=60 {
            many.advance(i * NS_PER_S, false);
        }
        assert_eq!(one.offset_ns(), many.offset_ns());
        assert_eq!(one.offset_ns(), -1_200_000);
    }

    /// A negative offset means the node believes it is earlier than it is.
    #[test]
    fn a_slow_clock_believes_it_is_earlier() {
        let mut c = ClockModel::new(-100.0);
        c.advance(0, false);
        c.advance(10 * NS_PER_S, false);
        assert!(c.believed_time(10 * NS_PER_S) < 10 * NS_PER_S);
        assert_eq!(c.error(10 * NS_PER_S), Duration::from_nanos(1_000_000));
    }

    /// The card names its one parameter as uncalibrated and carries the plan, so the model
    /// registers (rule R1) without asserting an oscillator figure no profile publishes.
    #[test]
    fn the_card_admits_the_drift_rate_is_uncalibrated() {
        let c = ClockModel::new(0.0);
        c.card().validate().expect("card validates");
        let todo: Vec<&str> = c.card().todo_calibrate().map(|p| p.name.as_str()).collect();
        assert_eq!(todo, vec!["drift_ppm"]);
    }
}
