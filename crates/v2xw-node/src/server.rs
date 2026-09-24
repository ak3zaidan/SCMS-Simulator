//! CPU and HSM servers: the modelled time a node spends doing cryptography
//! (06-node-models.md §2.1, 03-interfaces.md §8).
//!
//! This is the module the simulator exists for. A node that signs 10 BSMs a second on an
//! HSM whose datasheet says `<9 ms signing latency` is using 9 % of one engine; the same
//! node on a secure element that takes 60 ms is using 60 %, and a pseudonym change that
//! needs a second signature inside the same 100 ms window will not fit. A node that has to
//! verify 100 neighbours at 10 Hz needs a kilohertz of verification throughput, and the
//! profile set of §7 contains a real part that offers one hundred. None of that is visible
//! unless service time is real and the servers are finite, so here they are.
//!
//! The rule that keeps it honest is the same as everywhere else in the crate: a service
//! time comes from the profile or it does not exist. [`ProfileServiceModel::service_time`]
//! returns `None` for an operation the profile does not cost, and the caller has to decide
//! what to do about it, because the alternative — a plausible default — would silently set
//! the load of every run that forgot to configure it.

use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::ctx::NodeCtx;
use crate::profile::{HardwareProfile, RunsOn};

/// What kind of work a task is, for cost lookup and for the utilisation split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpClass {
    /// Producing a signature.
    Sign,
    /// Checking a signature.
    Verify,
    /// Hashing.
    Hash,
    /// Decoding an SPDU and applying the policy.
    Parse,
    /// Running a detector or a safety application.
    Application,
    /// Expanding a linkage-CRL i-period or sweeping an expired entry.
    CrlProcessing,
}

/// One unit of work, named so that its cost is a table lookup
/// (03-interfaces.md §8: `Task{queue, op, deadline}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpDescriptor {
    /// The operation id the profiles key their cost tables by, e.g. `ecdsa-p256-verify`.
    pub op: &'static str,
    /// What kind of work it is.
    pub class: OpClass,
    /// How many bytes it covers, where that matters.
    pub bytes: u32,
}

impl OpDescriptor {
    /// A signature verification over `bytes` bytes with the named primitive.
    pub const fn verify(op: &'static str, bytes: u32) -> Self {
        OpDescriptor {
            op,
            class: OpClass::Verify,
            bytes,
        }
    }

    /// A signature generation over `bytes` bytes.
    pub const fn sign(op: &'static str, bytes: u32) -> Self {
        OpDescriptor {
            op,
            class: OpClass::Sign,
            bytes,
        }
    }

    /// Non-cryptographic node work.
    pub const fn task(op: &'static str, class: OpClass) -> Self {
        OpDescriptor {
            op,
            class,
            bytes: 0,
        }
    }
}

/// How long an entity takes to do one thing, and how many at once
/// (03-interfaces.md §8).
///
/// Narrowed to [`NodeCtx`] rather than `Ctx` per build decision D12.2, and returning a
/// [`Duration`] rather than a `SimTime` per D11.1: a service time is a span, not an
/// instant.
pub trait ServiceModel: Model {
    /// How long `op` takes on this entity, or `None` when the profile costs no such
    /// operation.
    fn service_time(&mut self, ctx: &mut dyn NodeCtx, op: &OpDescriptor) -> Option<Duration>;

    /// How many of these run in parallel.
    fn servers(&self) -> u32;

    /// Where the work runs, which decides whether it charges the CPU or the HSM.
    fn runs_on(&self, op: &OpDescriptor) -> RunsOn;
}

/// Model id of the profile-driven service model.
pub const PROFILE_SERVICE_MODEL_ID: &str = "service-model/hardware-profile";

/// A [`ServiceModel`] that reads its costs out of a [`HardwareProfile`].
#[derive(Debug, Clone)]
pub struct ProfileServiceModel {
    profile: HardwareProfile,
    /// Cost of the non-cryptographic classes, when a scenario supplies one.
    app_task: Option<Duration>,
    /// The abstract compute tier: every operation the profile costs takes
    /// [`ProfileServiceModel::UNLIMITED_COST`] instead of its profiled time.
    unlimited: bool,
    card: ModelCard,
}

impl ProfileServiceModel {
    /// A service model over `profile`, with no cost for application tasks.
    ///
    /// Application task costs are `TODO: calibrate` in 06-node-models §2.1 ("per-model
    /// cost classes declared in their model cards with `TODO: calibrate` defaults measured
    /// on the reference laptop and scaled by DMIPS ratio"), and none of the ten profiles
    /// carries a DMIPS rating for the reference laptop to scale against, so the default is
    /// "not modelled" rather than a made-up microsecond count.
    pub fn new(profile: HardwareProfile) -> Self {
        let card = service_card(&profile);
        ProfileServiceModel {
            profile,
            app_task: None,
            unlimited: false,
            card,
        }
    }

    /// The service time of an operation at the abstract compute tier: one microsecond.
    ///
    /// Not zero, because a signature that completed at the instant the node decided to
    /// send would put the frame's MAC timer at the instant of the node phase that produced
    /// it, which the kernel refuses (02-architecture.md §5.1); one microsecond is the
    /// smallest interval that keeps it strictly after, and is what the engine already
    /// charges a frame whose profile publishes no signing rate.
    pub const UNLIMITED_COST: Duration = Duration::from_micros(1);

    /// The same model at the abstract compute tier (`nodes.compute_tier: abstract`):
    /// cryptography costs [`ProfileServiceModel::UNLIMITED_COST`], so no node is ever
    /// compute-bound and the verification queue never builds. An operation the profile
    /// does not cost still has no service time — the tier removes the bottleneck, it does
    /// not invent a primitive the device lacks.
    #[must_use]
    pub fn unlimited(mut self) -> Self {
        self.unlimited = true;
        self
    }

    /// Whether this model runs at the abstract compute tier.
    pub fn is_unlimited(&self) -> bool {
        self.unlimited
    }

    /// The same model with a scenario-supplied cost for application tasks.
    #[must_use]
    pub fn with_app_task_cost(mut self, cost: Duration) -> Self {
        self.app_task = Some(cost);
        self
    }

    /// The profile behind the costs.
    pub fn profile(&self) -> &HardwareProfile {
        &self.profile
    }
}

impl Model for ProfileServiceModel {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl ServiceModel for ProfileServiceModel {
    fn service_time(&mut self, _ctx: &mut dyn NodeCtx, op: &OpDescriptor) -> Option<Duration> {
        let cost = match op.class {
            OpClass::Sign | OpClass::Verify | OpClass::Hash => {
                self.profile.op_cost(op.op).map(|(d, _)| d)
            }
            OpClass::Parse | OpClass::Application | OpClass::CrlProcessing => self.app_task,
        };
        if self.unlimited {
            cost.map(|_| Self::UNLIMITED_COST)
        } else {
            cost
        }
    }

    fn servers(&self) -> u32 {
        // The CPU's core count is the only server count any profile publishes; the HSM's
        // is `hsm.servers`, which every profile carries as uncalibrated except
        // obu/cohda-mk5, where §7.1 states the single-server assumption explicitly.
        self.profile.cpu.cores.or(1).max(1)
    }

    fn runs_on(&self, op: &OpDescriptor) -> RunsOn {
        self.profile
            .op_cost(op.op)
            .map_or(RunsOn::Cpu, |(_, where_)| where_)
    }
}

fn service_card(profile: &HardwareProfile) -> ModelCard {
    let mut card = ModelCard::new(
        PROFILE_SERVICE_MODEL_ID,
        Family::ServiceModel,
        "0.1.0",
        format!(
            "Service times for one node, read out of the hardware profile `{}`; \
             an operation the profile does not cost has no service time here either.",
            profile.id
        ),
    );
    card.tier = vec![Tier::Medium, Tier::High];
    let mut p = Parameter::new(
        "app_task_us",
        "us",
        serde_json::Value::Null,
        Source::todo_calibrate("cost of a parse, detector or neighbour-table task"),
    );
    p.calibration = Some(
        "06-node-models §2.1 specifies these as per-model cost classes with TODO: calibrate \
         defaults 'measured on the reference laptop and scaled by DMIPS ratio'. Neither \
         the reference laptop nor a DMIPS rating for it exists yet, and six of the ten \
         shipped profiles publish no DMIPS either, so no default is asserted. Measure a \
         parse and a detector pass on the reference laptop with a cycle counter, record \
         its DMIPS, and scale."
            .to_string(),
    );
    card.parameters.push(p);
    card.sources.push(Source::new(
        SourceKind::Datasheet,
        format!("hardware profile {}@{}", profile.id, profile.version),
    ));
    card.assumptions.push(
        "An operation with no published cost has no modelled service time, rather than a \
         default: a default would set the load of every run that forgot to configure it, \
         invisibly."
            .to_string(),
    );
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card
}

/// A bank of `c` identical FIFO servers.
///
/// The `medium` tier of 06-node-models §2.1 is one CPU server and one HSM server; the
/// `high` tier is `c` cores. One type serves both because the only difference is `c`.
#[derive(Debug, Clone)]
pub struct ServerBank {
    name: &'static str,
    free_at: Vec<SimTime>,
    busy_ns: u64,
    completed: u32,
    window_start: SimTime,
}

/// When a submitted task starts and finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheduled {
    /// When a server picks it up.
    pub start: SimTime,
    /// When it is done.
    pub finish: SimTime,
    /// How long it waited between being offered and being started.
    pub wait: Duration,
}

impl ServerBank {
    /// A bank of `servers` servers, all free at `at`.
    ///
    /// A `servers` of zero is raised to one: a bank with no servers would accept work and
    /// never complete it, which models nothing and hides the configuration mistake.
    pub fn new(name: &'static str, servers: u32, at: SimTime) -> Self {
        ServerBank {
            name,
            free_at: vec![at; servers.max(1) as usize],
            busy_ns: 0,
            completed: 0,
            window_start: at,
        }
    }

    /// The bank's name, for records.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// How many servers.
    pub fn servers(&self) -> usize {
        self.free_at.len()
    }

    /// The instant the first server frees up — when a task offered now would start, if it
    /// arrived no later than this.
    ///
    /// What a FIFO waiting line in front of the bank needs to simulate itself in continuous
    /// time: the head of the line starts at `max(its arrival, earliest_free())`, and it is
    /// in the queue (not in service) until then.
    pub fn earliest_free(&self) -> SimTime {
        self.free_at.iter().copied().min().unwrap_or(0)
    }

    /// Offers one task of length `duration`, arriving at `arrival`.
    ///
    /// The task goes to the server that frees up first; ties go to the lowest-indexed
    /// server, so the assignment is a function of the arrival sequence alone and does not
    /// depend on iteration order or on the thread the phase ran on.
    pub fn submit(&mut self, arrival: SimTime, duration: Duration) -> Scheduled {
        let mut best = 0usize;
        for i in 1..self.free_at.len() {
            if self.free_at[i] < self.free_at[best] {
                best = i;
            }
        }
        let start = arrival.max(self.free_at[best]);
        let finish = duration.after(start);
        self.free_at[best] = finish;
        self.busy_ns = self.busy_ns.saturating_add(duration.as_nanos());
        self.completed = self.completed.saturating_add(1);
        Scheduled {
            start,
            finish,
            wait: Duration::between(arrival, start),
        }
    }

    /// The fraction of the window every server was busy, in per mille, averaged over the
    /// servers and saturated at 1000.
    ///
    /// Per mille because that is the unit the telemetry record uses (`cpu_util_pm`,
    /// `hsm_util_pm`, §3.5.2 offsets 148 and 150).
    pub fn utilisation_pm(&self, now: SimTime) -> u16 {
        let window = now.saturating_sub(self.window_start);
        if window == 0 {
            return 0;
        }
        let capacity = (window as f64) * self.free_at.len() as f64;
        let frac = (self.busy_ns as f64) / capacity;
        // Quantised before the comparison and the cast, per build decision D10: the value
        // reaches an exported artefact and a threshold in the HUD.
        let pm = v2xw_core::math::quantize_to(frac * 1000.0, 1.0);
        if pm <= 0.0 {
            0
        } else if pm >= 1000.0 {
            1000
        } else {
            pm as u16
        }
    }

    /// How many tasks completed in this window.
    pub fn completed(&self) -> u32 {
        self.completed
    }

    /// Total busy time in this window.
    pub fn busy(&self) -> Duration {
        Duration::from_nanos(self.busy_ns)
    }

    /// Whether every server is idle at `now`.
    pub fn is_idle(&self, now: SimTime) -> bool {
        self.free_at.iter().all(|&t| t <= now)
    }

    /// Clears the window counters. The servers keep whatever work they are mid-way
    /// through, because a reporting boundary is not a reset of the hardware.
    pub fn reset_window(&mut self, now: SimTime) {
        self.busy_ns = 0;
        self.completed = 0;
        self.window_start = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::NodeRuntimeCtx;
    use crate::profile::HsmKind;
    use v2xw_core::rng::RngRegistry;
    use v2xw_core::time::NS_PER_MS;

    fn profile(id: &str) -> HardwareProfile {
        crate::profiles::get(id).expect("shipped profile").clone()
    }

    /// The reference OBU's published sign latency is 9 ms and its verify rate is a
    /// published lower bound with no latency, so the verify service time is the derived
    /// reciprocal. Both come from the profile; neither is written here.
    #[test]
    fn service_times_come_from_the_profile() {
        let reg = RngRegistry::new(0);
        let mut ctx = NodeRuntimeCtx::new(0, &reg);
        let mut sm = ProfileServiceModel::new(profile(crate::profiles::REFERENCE_OBU));

        let sign = sm
            .service_time(&mut ctx, &OpDescriptor::sign("ecdsa-p256-sign", 64))
            .expect("published");
        assert_eq!(sign, Duration::from_millis(9));
        assert_eq!(
            sm.runs_on(&OpDescriptor::sign("ecdsa-p256-sign", 64)),
            RunsOn::Hsm
        );

        let verify = sm
            .service_time(&mut ctx, &OpDescriptor::verify("ecdsa-p256-verify", 300))
            .expect("derived from the published rate");
        assert_eq!(verify, Duration::from_micros(400));
        assert_eq!(
            sm.runs_on(&OpDescriptor::verify("ecdsa-p256-verify", 300)),
            RunsOn::Accelerator
        );
    }

    /// The abstract compute tier: everything the profile costs takes a microsecond, and
    /// what it does not cost still has no service time.
    #[test]
    fn the_unlimited_tier_costs_a_microsecond_and_invents_nothing() {
        let reg = RngRegistry::new(0);
        let mut ctx = NodeRuntimeCtx::new(0, &reg);
        let mut sm = ProfileServiceModel::new(profile(crate::profiles::REFERENCE_OBU)).unlimited();
        assert!(sm.is_unlimited());
        let sign = sm.service_time(&mut ctx, &OpDescriptor::sign("ecdsa-p256-sign", 64));
        assert_eq!(sign, Some(ProfileServiceModel::UNLIMITED_COST));
        let verify = sm.service_time(&mut ctx, &OpDescriptor::verify("ecdsa-p256-verify", 300));
        assert_eq!(verify, Some(ProfileServiceModel::UNLIMITED_COST));
        assert_eq!(
            sm.service_time(&mut ctx, &OpDescriptor::sign("no-such-primitive", 64)),
            None
        );
    }

    /// Rule H2 in the cost path: a profile with no security hardware charges the software
    /// table, and the work lands on the CPU.
    #[test]
    fn a_profile_without_an_hsm_charges_the_cpu() {
        let reg = RngRegistry::new(0);
        let mut ctx = NodeRuntimeCtx::new(0, &reg);
        let p = profile("obu/generic-automotive-soc-no-hsm");
        assert_eq!(p.hsm.kind, HsmKind::None);
        let mut sm = ProfileServiceModel::new(p);
        let op = OpDescriptor::verify("ecdsa-p256-verify", 300);
        assert_eq!(
            sm.service_time(&mut ctx, &op),
            Some(Duration::from_micros(645))
        );
        assert_eq!(sm.runs_on(&op), RunsOn::Cpu);
        assert_eq!(sm.servers(), 4);
    }

    /// An operation nobody costed has no service time. This is the behaviour that keeps
    /// an unconfigured run visibly unconfigured.
    #[test]
    fn an_uncosted_operation_has_no_service_time() {
        let reg = RngRegistry::new(0);
        let mut ctx = NodeRuntimeCtx::new(0, &reg);
        let mut sm = ProfileServiceModel::new(profile("obu/cohda-mk5"));
        // The MK5's SXF1700 sign figures are NOT PUBLISHED and no software fallback is
        // measured for the i.MX6 DL either, so there is nothing to charge.
        assert_eq!(
            sm.service_time(&mut ctx, &OpDescriptor::sign("ecdsa-p256-sign", 64)),
            None
        );
        // Nor is an application task costed until a scenario supplies one.
        assert_eq!(
            sm.service_time(&mut ctx, &OpDescriptor::task("parse", OpClass::Parse)),
            None
        );
        let mut sm = sm.with_app_task_cost(Duration::from_micros(50));
        assert_eq!(
            sm.service_time(&mut ctx, &OpDescriptor::task("parse", OpClass::Parse)),
            Some(Duration::from_micros(50))
        );
    }

    /// One server, three back-to-back 9 ms signatures: the third waits 18 ms, which is the
    /// queueing delay a node that cannot keep up actually experiences.
    #[test]
    fn a_single_server_makes_work_queue() {
        let mut bank = ServerBank::new("hsm", 1, 0);
        let d = Duration::from_millis(9);
        let a = bank.submit(0, d);
        let b = bank.submit(0, d);
        let c = bank.submit(0, d);
        assert_eq!(a.wait, Duration::ZERO);
        assert_eq!(b.wait, Duration::from_millis(9));
        assert_eq!(c.wait, Duration::from_millis(18));
        assert_eq!(c.finish, 27 * NS_PER_MS);
        assert_eq!(bank.completed(), 3);
    }

    /// Four servers take four simultaneous tasks without queueing, and the utilisation is
    /// the average over the bank rather than over one server.
    #[test]
    fn a_bank_spreads_work_and_averages_its_utilisation() {
        let mut bank = ServerBank::new("cpu", 4, 0);
        for _ in 0..4 {
            assert_eq!(
                bank.submit(0, Duration::from_millis(100)).wait,
                Duration::ZERO
            );
        }
        // 4 x 100 ms of work over a 1 s window across 4 servers is 10 %.
        assert_eq!(bank.utilisation_pm(v2xw_core::time::NS_PER_S), 100);
        // The same work on one server is 40 %.
        let mut one = ServerBank::new("cpu", 1, 0);
        for _ in 0..4 {
            one.submit(0, Duration::from_millis(100));
        }
        assert_eq!(one.utilisation_pm(v2xw_core::time::NS_PER_S), 400);
    }

    /// Utilisation saturates rather than reporting more than 100 %, which a bank given
    /// more work than the window can hold would otherwise do.
    #[test]
    fn utilisation_saturates() {
        let mut bank = ServerBank::new("hsm", 1, 0);
        for _ in 0..20 {
            bank.submit(0, Duration::from_millis(100));
        }
        assert_eq!(bank.utilisation_pm(v2xw_core::time::NS_PER_S), 1000);
    }
}
