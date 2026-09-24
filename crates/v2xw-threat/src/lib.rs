//! `v2xw-threat` — the attacks and the detectors: the subject matter this simulator
//! exists to study.
//!
//! # What is here
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | What an attacker may see and do; when it acts | [`capability`] | 07-threats-and-detection.md §1 |
//! | The attacker interface, the action vocabulary, the falsified-label rule | [`attack`] | 03-interfaces.md §9, 07-threats §2, §4 |
//! | The 28 legacy attack renderings plus selective dropping | [`attack_legacy`] | 07-threats §2.1 |
//! | Ghost vehicles, relay replay, oversized flooding, region misuse, phantom CPM, jamming | [`attack_ext`] | 07-threats §2.2 |
//! | The compromised road-side unit, on the air and on the reporting path | [`attack_rsu`] | 07-threats §2.2 |
//! | Misbehaviour-report poisoning | [`poison`] | 07-threats §2.1, §2.2 |
//! | The passive privacy observer and its four metrics | [`privacy`] | 07-threats §6 |
//! | The whole catalogue in one namespace | [`catalog`] | 07-threats §2 |
//! | The belief-side inputs every model here consumes | [`obs`] | 03-interfaces.md §1, §9 |
//! | The legacy twelve detectors, two gated checks, one soft feature | [`detect`] | 07-threats §3.1 |
//! | The TS 103 759 classes 1–5, the F2MD checks, the perception cross-check | [`ts103759`] | 07-threats §3.1, 04-models §14 |
//! | The misbehaviour report and its forged variant | [`report`] | 07-threats §2.1, §3.2 |
//! | The misbehaviour-authority pipeline | [`ma`] | 07-threats §3.2 |
//! | Two-authority identity resolution, and what it is worth | [`resolve`] | 07-threats §3.2 |
//! | The records the metrics crate scores | [`records`] | 08-measurement-and-data.md §2.4, §2.6 |
//! | The narrow context a node-resident plug-in gets | [`ctx`] | 03-interfaces.md §1.1 |
//! | Citation helpers | [`cards`] | 03-interfaces.md §12 |
//!
//! # The firewall
//!
//! Three invariants govern this crate, and each is enforced by a *type* rather than by a
//! convention, because the failure mode they prevent is silent:
//!
//! * **I-T1** — an attacker receives no world and no actor index. [`attack::AttackerView`]
//!   has neither, and no [`v2xw_core::ids::ActorId`] either.
//! * **I-T2** — detectors and the authority pipeline run on belief only. Every input to
//!   [`detect::Detector::on_message`] is an [`obs::ObservedMessage`], the node's own
//!   [`obs::SelfBelief`] and the node's own map store.
//! * **I-T3** — every action that changes bytes on the air is logged on a ground-truth
//!   channel with the true actor id. [`attack::log_actions`] writes it, and it is **host
//!   side**: it takes the actor id as an argument precisely because nothing in the
//!   attacker can supply one.
//!
//! A detector that reads ground truth does not crash, does not look wrong and does not
//! fail a test. It simply reports a detection rate that no real receiver could achieve,
//! and nobody reports a result that is too good. So the defence is the argument type, and
//! [`ctx::ThreatCtx`] exists because [`v2xw_core::ctx::Ctx`] would have carried `world()`
//! into every signature here.
//!
//! # The seam
//!
//! `v2xw-node`'s `VerifiedMessage` and `NeighborTable` and `v2xw-engine`'s scenario types
//! are not named anywhere in this crate. The belief shapes in [`obs`] mirror them field
//! for field, and the engine supplies them through a `From` adapter on its side of the
//! seam.
//!
//! ## What the engine has to call
//!
//! `tests/common/sim.rs` is a **running** attack-in-the-loop simulation built on the same
//! models `v2xw_engine::wiring` selects — the procedural world, `NativeMobility`,
//! `GaussMarkovGnss`, `LogDistanceShadowing` + `NakagamiFading`, the `PerModel`, and
//! `ObuRuntime` on the reference profile — so the two calls the engine's own loop is
//! missing are exercised rather than described. They are:
//!
//! 1. **Between `ObuRuntime::step` and the frame going on the air.** The engine's
//!    `run::Engine::launch` already takes the node's belief and turns it into a frame's
//!    `claimed_pos`/`claimed_speed_mps`/`claimed_heading_rad`. An attacker is
//!    [`attack::Attacker::act`] called on an [`attack::Emission`] built from that belief,
//!    immediately before the frame is constructed; the returned actions go to
//!    [`attack::log_actions`] with the `ActorId` the engine knows and the attacker does
//!    not. The emission's `repetitions`, `signature_valid`, `cert_valid_from/to`,
//!    `station_type`, `suppressed` and `ghosts` are what the frame must then carry, and
//!    `ghosts` are extra frames signed with the attacker's other pseudonyms.
//! 2. **On each node's delivered messages.** [`detect::Detector::on_message`] takes an
//!    [`obs::ObservedMessage`], the node's own [`obs::SelfBelief`] and the node's map
//!    store, and writes its own `det.observation` records. Four of
//!    `v2xw_node::VerifiedMessage`'s fields are not on it — the repetition count, the
//!    certificate validity window, the declared station type and the broadcast position
//!    confidence — so the frame has to carry them alongside; the harness joins them back
//!    by `(signer, generation time)`.
//!
//! Two things the harness found that the engine will meet as soon as it makes those calls
//! are recorded where they belong: [`detect`]'s module documentation, under "Measured
//! against the legacy engine".
//!
//! ## The four calls the Phase 4 models add
//!
//! The same shape: the model decides, the host applies and pays.
//!
//! 3. **On a frame the node captured, and on the energy it emits.**
//!    [`attack_ext::ExtendedAttacker`] fills three more fields of the emission the host
//!    must honour: `payload_bytes` (the oversized flood), `cert_region` (region misuse)
//!    and `perceived` (a phantom CPM), plus `raw_energy` — a jam burst the radio has to
//!    enter into its interference sums — and a ghost with `replayed` set, which the host
//!    puts on the air **verbatim** rather than re-signing, because that is what makes a
//!    relayed frame verify.
//! 4. **On a report crossing a road-side unit.** [`attack_rsu::CompromisedRsu::on_forward`]
//!    returns [`attack_rsu::ForwardDecision`] for each report handed to it: forward, drop,
//!    or forward this other one instead.
//! 5. **After an attacker acts.** [`poison::ReportPoisoner::take_reports`] hands over the
//!    reports it filed, for the host to submit through the reporting transport with that
//!    transport's delay and byte cost.
//! 6. **On each reception, at an observer node.**
//!    [`privacy::PrivacyObserver::on_message`] takes the same arguments a detector's does,
//!    plus [`privacy::PrivacyObserver::sweep`] on a timer and
//!    [`privacy::PrivacyObserver::finish`] at the end of the run, which is what closes the
//!    last tracking-duration samples.
//!
//! Two of those need something the run declares from the ground-truth side, and neither
//! model may infer it: [`resolve::TwoAuthorityResolution::declare_linkage`] (which
//! authority can resolve which pseudonym) and the digest-to-actor map the metrics crate
//! already takes through `DetectionProvider::declare_subject`, which is what turns a
//! [`records::PrivacyLinkClaim`] into a linkability rate.
//!
//! # Conformance
//!
//! The legacy renderings and thresholds are reproduced exactly, because the legacy corpus
//! was generated with them (07-threats §2.1, §3.1). `tests/legacy_conformance.rs` asserts
//! every ported constant against a value **read out of the legacy source at test time**,
//! not against a number somebody remembered — so a silent drift in either direction fails
//! the build rather than the dataset.
//!
//! ```
//! use v2xw_threat::attack::{AttackKind, Attacker, Emission, HonestClaim, AttackerView};
//! use v2xw_threat::attack_legacy::{LegacyAttacker, LegacyAttackerParams};
//! use v2xw_threat::capability::{AttackSchedule, Capabilities};
//! use v2xw_threat::ctx::CollectingCtx;
//! use v2xw_threat::obs::SelfBelief;
//! use v2xw_core::ids::NodeId;
//!
//! let mut ctx = CollectingCtx::new(1);
//! let mut a = LegacyAttacker::new(
//!     NodeId::new(1),
//!     LegacyAttackerParams::new(AttackKind::ConstPosOffset),
//!     Capabilities::insider(20),
//!     AttackSchedule { from: 0, to: u64::MAX, ..AttackSchedule::default() },
//!     Vec::new(),
//! );
//! let honest = HonestClaim { x_m: 0.0, y_m: 0.0, speed_mps: 15.0, heading_rad: 0.0 };
//! let me = SelfBelief { node: NodeId::new(1), believed_time: 10_000_000_000,
//!                       x_m: 0.0, y_m: 0.0, radio_range_m: 500.0 };
//! let view = AttackerView { own_rx: &[], own_credentials: &[], crl_revocations_seen: None,
//!                           own_belief: me, honest, believed_time: 10_000_000_000 };
//! let mut out = Emission::honest([0; 8], honest, 10_000_000_000, 0, u64::MAX);
//! a.act(&mut ctx, &view, &mut out);
//! // The legacy ConstPosOffset rendering: +25 m on both axes at intensity 1.
//! assert_eq!((out.x_m, out.y_m), (25.0, 25.0));
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod attack;
pub mod attack_ext;
pub mod attack_legacy;
pub mod attack_rsu;
pub mod capability;
pub mod cards;
pub mod catalog;
pub mod ctx;
pub mod detect;
pub mod ma;
pub mod obs;
pub mod poison;
pub mod privacy;
pub mod records;
pub mod report;
pub mod resolve;
pub mod ts103759;

pub use attack::{
    AttackAction, AttackFamily, AttackKind, Attacker, AttackerView, Emission, EventClaim,
    HonestClaim, InfraClaim, JamProfile, MAX_MSDU_BYTES, RawEnergy, falsified_count, is_falsified,
    is_falsified_extended, log_actions,
};
pub use attack_ext::{ExtendedAttackKind, ExtendedAttacker, ExtendedAttackerParams, LaneHint};
pub use attack_legacy::{LegacyAttacker, LegacyAttackerParams, Magnitudes};
pub use attack_rsu::{CompromisedRsu, ForwardDecision, RsuAttackKind, RsuAttackParams};
pub use capability::{
    AttackSchedule, Capabilities, CoalitionId, CredentialAccess, Knowledge, RadioCaps,
};
pub use catalog::CatalogEntry;
pub use ctx::{CollectingCtx, ThreatCtx, ThreatCtxExt};
pub use detect::{
    Detector, DetectorCost, DetectorId, DetectorParams, Fingerprint, Legacy12, Observation, Verdict,
};
pub use ma::{LegacyWindow, MaAction, MaParams, MaPipeline};
pub use obs::{
    DiscSensor, EnvelopeExtras, LocalEnvironment, LocalPerception, NoMap, NoPerception,
    ObservedKind, ObservedMessage, PeerBelief, PerceivedObject, RegionId, SelfBelief, SensedObject,
    StationType, VerificationState,
};
pub use poison::{PoisonParams, ReportPoisoner};
pub use privacy::{LinkOutcome, ObserverParams, PrivacyObserver};
pub use records::{
    DetObservation, GtAttackAction, MaCaseRecord, MaDecisionRecord, MaReportRecord,
    PrivacyLinkClaim, PrivacyTrackSegment,
};
pub use report::{CertValidity, Evidence, ForgeryProfile, MisbehaviourReport, forge};
pub use resolve::{Authority, Case, CaseOutcome, ResolutionParams, TwoAuthorityResolution};
pub use ts103759::{
    CrossCheckInputs, ObservationClass, Ts103759Check, Ts103759Params, Ts103759Suite,
    TsObservation, TsVerdict,
};
