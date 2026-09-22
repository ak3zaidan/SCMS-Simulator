//! When the node transmits: BSM at 10 Hz and CAM on the EN 302 637-2 triggers.
//!
//! The rules themselves live in `v2xw-msg` — [`v2xw_msg::generator::CamTriggerState`] is
//! the EN 302 637-2 §6.1.3 state machine and [`v2xw_msg::generator::BsmGenerator`] the
//! J2945/1 10 Hz cadence — and this module is the node-side wiring. It is a thin module
//! with one job, and the job is the one thing a generator can get catastrophically wrong:
//! **what it reads.**
//!
//! # The input is the belief, and only the belief
//!
//! [`MessageSchedule::due`] takes a [`PositionEstimate`] and a believed instant. It cannot
//! take ground truth, because ground truth is not in its signature and this crate has no
//! way to reach it (see [`crate::ctx`] and [`crate::firewall`]).
//!
//! That is not pedantry. A CAM is triggered when the vehicle has moved more than 4 m,
//! turned more than 4 degrees or changed speed by more than 0.5 m/s. Evaluate those
//! thresholds against the truth and the generation rate is a clean function of the
//! vehicle's motion; evaluate them against a GNSS belief with metre-scale noise and the
//! rate rises, because noise crosses a 4 m threshold on its own. The second is what
//! happens on a road. A simulator that produced the first would under-report channel load
//! in exactly the conditions — poor fix, urban canyon — where load matters most, and the
//! error would be invisible because both numbers look reasonable.
//!
//! The same argument applies to the clock: the intervals are measured on the sender's
//! believed time, so a node whose clock has been stepped generates at a visibly wrong
//! rate rather than at a correct one.
//!
//! # Provenance of the trigger block
//!
//! The CAM rule set is EN 302 637-2 V1.4.1 §6.1.3: dynamics triggers at Δposition > 4 m,
//! Δheading > 4°, Δspeed > 0.5 m/s, floored at `T_GenCamMin` (100 ms) and heart-beating
//! at `T_GenCamMax` (1,000 ms). A verified reference implementation of exactly this block
//! exists in the retired legacy engine at `legacy/reference/jvm/ScmsBeaconApp.java`
//! lines 140-154, and 01-inventory.md §3 singles it out as "a correct ETSI EN 302 637-2
//! CAM trigger block (heading > 4°, position > 4 m, speed > 0.5 m/s, 100 ms-1 s) worth
//! citing when the CAM generator is written". It is cited, and two of its details are
//! deliberately *not* reproduced:
//!
//! * the Java block compares the heading difference in degrees against `4.0` while
//!   reading a heading in degrees; this crate works in ENU radians throughout (build
//!   decision D6) and the threshold is `CamGenParams::FOUR_DEGREES_RAD`;
//! * the Java block reads `p.getX()`/`p.getY()` from the simulator's own position, which
//!   is ground truth. That is the legacy defect 01-inventory §3.3 records and the reason
//!   the firewall exists. The rule set is worth citing; the input is not.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::time::{Duration, SimTime};
use v2xw_msg::MsgType;
use v2xw_msg::generator::{
    BsmGenParams, BsmGenerator, CamDynamics, CamGenParams, CamTriggerState, DccState, GenReason,
    GenRequest,
};

/// Which message services a node runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceSet {
    /// Generate CAMs on the EN 302 637-2 triggers.
    pub cam: bool,
    /// Generate BSMs at the J2945/1 cadence.
    pub bsm: bool,
}

impl ServiceSet {
    /// The ETSI stack: CAM only.
    pub const ETSI: ServiceSet = ServiceSet {
        cam: true,
        bsm: false,
    };
    /// The SAE stack: BSM only.
    pub const SAE: ServiceSet = ServiceSet {
        cam: false,
        bsm: true,
    };
    /// Both, for a dual-stack unit.
    pub const BOTH: ServiceSet = ServiceSet {
        cam: true,
        bsm: true,
    };
}

/// The node's message-generation timers.
#[derive(Debug, Clone)]
pub struct MessageSchedule {
    services: ServiceSet,
    cam: CamTriggerState,
    bsm: BsmGenerator,
    generated: u32,
}

impl MessageSchedule {
    /// A schedule running `services` with the standards' own defaults.
    pub fn new(services: ServiceSet) -> Self {
        MessageSchedule {
            services,
            cam: CamTriggerState::new(CamGenParams::en302637_2()),
            bsm: BsmGenerator::new(BsmGenParams::j2945_1()),
            generated: 0,
        }
    }

    /// The same schedule with non-default parameters.
    #[must_use]
    pub fn with_params(mut self, cam: CamGenParams, bsm: BsmGenParams) -> Self {
        self.cam = CamTriggerState::new(cam);
        self.bsm = BsmGenerator::new(bsm);
        self
    }

    /// How often the engine must call [`MessageSchedule::due`].
    ///
    /// `T_CheckCamGen` in EN 302 637-2, which the standard requires to be no greater than
    /// `T_GenCamMin`: a check interval coarser than the minimum period could not honour
    /// the minimum. The BSM cadence is checked at the same interval, and 100 ms is both
    /// standards' floor, so one timer serves both.
    pub fn check_interval(&self) -> Duration {
        self.cam.params().t_gen_cam_min
    }

    /// The CAM state machine, for a detector that wants to ask whether a peer's CAM
    /// would have been triggered.
    pub fn cam(&self) -> &CamTriggerState {
        &self.cam
    }

    /// The `msgCnt` the next BSM will carry.
    pub fn bsm_msg_count(&self) -> u8 {
        self.bsm.msg_count()
    }

    /// How many messages this schedule has asked for.
    pub fn generated(&self) -> u32 {
        self.generated
    }

    /// Clears the window counter.
    pub fn reset_window(&mut self) {
        self.generated = 0;
    }

    /// What, if anything, to send now.
    ///
    /// `believed_now` is the node's own clock and `belief` its own position estimate.
    /// Neither argument can be ground truth, and that is the module's entire contract.
    pub fn due(
        &mut self,
        believed_now: SimTime,
        belief: &PositionEstimate,
        dcc: &DccState,
    ) -> Vec<GenRequest> {
        let mut out = Vec::new();
        if self.services.cam {
            let dynamics = CamDynamics::from_estimate(belief);
            if let Some(d) = self.cam.check(believed_now, &dynamics, dcc) {
                out.push(GenRequest {
                    msg_type: MsgType::Cam,
                    reason: d.reason,
                    include_low_frequency: d.include_low_frequency,
                    at: believed_now,
                });
            }
        }
        if self.services.bsm
            && let Some(reason) = self.bsm.check(believed_now, dcc)
        {
            out.push(GenRequest {
                msg_type: MsgType::Bsm,
                reason,
                // J2735 Part II is the BSM's low-cadence content; this generator emits
                // Part I only, which 04-models.md §8.1 records as the modelled subset.
                include_low_frequency: false,
                at: believed_now,
            });
        }
        self.generated = self.generated.saturating_add(out.len() as u32);
        out
    }
}

/// Whether a request was triggered by the vehicle's dynamics rather than by a timer.
pub fn is_dynamics_triggered(r: &GenRequest) -> bool {
    matches!(r.reason, GenReason::Dynamics(t) if t.any())
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::geom::Vec3;
    use v2xw_core::time::{NS_PER_MS, NS_PER_S};

    fn belief_at(pos: Vec3, heading: f64, speed: f64) -> PositionEstimate {
        let mut p = PositionEstimate::no_fix(0);
        p.pos = pos;
        p.heading_rad = heading;
        p.vel = Vec3::new(
            speed * v2xw_core::math::cos(heading),
            speed * v2xw_core::math::sin(heading),
            0.0,
        );
        p.fix = v2xw_core::belief::FixQuality::ThreeD;
        p
    }

    /// A stationary node on the SAE stack transmits at 10 Hz and no faster, which is the
    /// J2945/1 cadence.
    #[test]
    fn bsm_runs_at_ten_hertz() {
        let mut s = MessageSchedule::new(ServiceSet::SAE);
        let b = belief_at(Vec3::ZERO, 0.0, 0.0);
        let dcc = DccState::UNRESTRICTED;
        let mut sent = 0;
        // Check every 10 ms for one second: 100 checks, 10 transmissions plus the first.
        for k in 0..=100u64 {
            sent += s.due(k * 10 * NS_PER_MS, &b, &dcc).len();
        }
        assert_eq!(sent, 11, "the first, then one per 100 ms");
        assert_eq!(s.generated(), 11);
    }

    /// A stationary node on the ETSI stack heart-beats at `T_GenCamMax`, one per second,
    /// because no dynamics trigger fires.
    #[test]
    fn a_stationary_node_heart_beats_at_one_hertz() {
        let mut s = MessageSchedule::new(ServiceSet::ETSI);
        let b = belief_at(Vec3::ZERO, 0.0, 0.0);
        let dcc = DccState::UNRESTRICTED;
        let mut sent = 0;
        for k in 0..=50u64 {
            sent += s.due(k * 100 * NS_PER_MS, &b, &dcc).len();
        }
        assert_eq!(sent, 6, "the first, then one per second for five seconds");
    }

    /// The position trigger: 4 m of movement inside one heart-beat period fires trigger 1
    /// rather than waiting for `T_GenCamMax`. EN 302 637-2 §6.1.3, and the block at
    /// `legacy/reference/jvm/ScmsBeaconApp.java:140-154`.
    #[test]
    fn four_metres_of_movement_triggers_a_cam() {
        let mut s = MessageSchedule::new(ServiceSet::ETSI);
        let dcc = DccState::UNRESTRICTED;
        assert_eq!(s.due(0, &belief_at(Vec3::ZERO, 0.0, 0.0), &dcc).len(), 1);

        // 3.9 m at t = 200 ms: past T_GenCamMin, under the threshold, no CAM.
        let near = belief_at(Vec3::new(3.9, 0.0, 0.0), 0.0, 0.0);
        assert!(s.due(200 * NS_PER_MS, &near, &dcc).is_empty());

        // 4.1 m at t = 300 ms: trigger 1.
        let far = belief_at(Vec3::new(4.1, 0.0, 0.0), 0.0, 0.0);
        let out = s.due(300 * NS_PER_MS, &far, &dcc);
        assert_eq!(out.len(), 1);
        assert!(is_dynamics_triggered(&out[0]));
        assert!(matches!(
            out[0].reason,
            GenReason::Dynamics(t) if t.position && !t.heading && !t.speed
        ));
    }

    /// The heading and speed triggers, each on its own, with the other two quantities
    /// held still so the attribution is unambiguous.
    #[test]
    fn heading_and_speed_each_trigger_on_their_own() {
        let dcc = DccState::UNRESTRICTED;

        let mut s = MessageSchedule::new(ServiceSet::ETSI);
        s.due(0, &belief_at(Vec3::ZERO, 0.0, 10.0), &dcc);
        let five_degrees = 5.0 * core::f64::consts::PI / 180.0;
        let out = s.due(
            200 * NS_PER_MS,
            &belief_at(Vec3::ZERO, five_degrees, 10.0),
            &dcc,
        );
        assert!(matches!(
            out[0].reason,
            GenReason::Dynamics(t) if t.heading && !t.position
        ));

        let mut s = MessageSchedule::new(ServiceSet::ETSI);
        s.due(0, &belief_at(Vec3::ZERO, 0.0, 10.0), &dcc);
        let out = s.due(200 * NS_PER_MS, &belief_at(Vec3::ZERO, 0.0, 10.6), &dcc);
        assert!(matches!(
            out[0].reason,
            GenReason::Dynamics(t) if t.speed && !t.position && !t.heading
        ));
    }

    /// `T_GenCamMin` is a floor even when the vehicle is manoeuvring hard: two 10 m jumps
    /// 50 ms apart produce one CAM, not two.
    #[test]
    fn t_gen_cam_min_floors_the_rate() {
        let mut s = MessageSchedule::new(ServiceSet::ETSI);
        let dcc = DccState::UNRESTRICTED;
        s.due(0, &belief_at(Vec3::ZERO, 0.0, 0.0), &dcc);
        assert!(
            s.due(
                50 * NS_PER_MS,
                &belief_at(Vec3::new(10.0, 0.0, 0.0), 0.0, 0.0),
                &dcc
            )
            .is_empty()
        );
        assert_eq!(
            s.due(
                100 * NS_PER_MS,
                &belief_at(Vec3::new(20.0, 0.0, 0.0), 0.0, 0.0),
                &dcc
            )
            .len(),
            1
        );
    }

    /// DCC raises the floor: with `t_off` at 500 ms a manoeuvring node cannot transmit at
    /// 10 Hz however much it moves.
    #[test]
    fn dcc_raises_the_floor_above_the_standards_minimum() {
        let mut s = MessageSchedule::new(ServiceSet::BOTH);
        let dcc = DccState {
            t_off: Duration::from_millis(500),
            cbr: Some(0.7),
        };
        s.due(0, &belief_at(Vec3::ZERO, 0.0, 0.0), &dcc);
        let mut sent = 0;
        for k in 1..=10u64 {
            let moved = belief_at(Vec3::new(k as f64 * 10.0, 0.0, 0.0), 0.0, 0.0);
            sent += s.due(k * 100 * NS_PER_MS, &moved, &dcc).len();
        }
        // One second of checks, two services, one transmission each per 500 ms.
        assert_eq!(sent, 4);
    }

    /// The generation clock is the node's, so a node whose clock has been stepped forward
    /// generates a message it would not otherwise have been due for. A simulator reading
    /// the true clock would show nothing at all here.
    #[test]
    fn a_stepped_clock_changes_the_generation_pattern() {
        let dcc = DccState::UNRESTRICTED;
        let b = belief_at(Vec3::ZERO, 0.0, 0.0);

        let mut honest = MessageSchedule::new(ServiceSet::SAE);
        honest.due(0, &b, &dcc);
        assert!(honest.due(50 * NS_PER_MS, &b, &dcc).is_empty());

        let mut stepped = MessageSchedule::new(ServiceSet::SAE);
        stepped.due(0, &b, &dcc);
        // The same true instant, but this node believes a second has passed.
        assert_eq!(stepped.due(50 * NS_PER_MS + NS_PER_S, &b, &dcc).len(), 1);
    }
}
