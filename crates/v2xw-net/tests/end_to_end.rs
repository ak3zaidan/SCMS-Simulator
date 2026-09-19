//! Integration tests: the crate through its public API, from a downstream crate's position.
//!
//! Three things can only be checked from outside the crate, and all three are conformance
//! items of 03-interfaces.md §17:
//!
//! 1. **A downstream crate can implement [`Ctx`] and use these seams.** The context is a
//!    type parameter, so the engine crate supplies the concrete type; this file plays that
//!    role, which is why it defines its own context instead of reaching for an internal one.
//! 2. **Dyn-compatibility.** `Box<dyn NetLayer<C>>`, `Box<dyn Fragmenter<C>>` and
//!    `Arc<dyn Model + Send + Sync>` all have to build, because in-process plug-ins are
//!    trait objects (ADR 0007 §8).
//! 3. **Invariant I-N1 across the layers.** Header octets, fragmentation overhead and
//!    payload all reach exactly one accounting bucket once, which is only visible when the
//!    network layer, the fragmenter and the ledger are used together.

use std::sync::Arc;

use v2xw_core::card::Family;
use v2xw_core::ctx::{Ctx, ErasedRecord, Visibility};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::ids::{NodeId, SduId};
use v2xw_core::math;
use v2xw_core::model::ModelHandle;
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::{NS_PER_MS, SimTime};

use v2xw_net::accounting::{Bucket, ByteLedger, BytesOnWire};
use v2xw_net::amplification::{AmplificationMeter, FragmentLoss};
use v2xw_net::frag::cert_cycle::{CertCyclePartialHybrid, DSRC_PAYLOAD_CAP_BYTES};
use v2xw_net::frag::facilities::FacilitiesSegmentation;
use v2xw_net::frag::generic::GenericSduFragmenter;
use v2xw_net::frag::none::NoneFragmenter;
use v2xw_net::frag::{Fragmenter, ReassemblyOutcome};
use v2xw_net::gn::GnBtpNetLayer;
use v2xw_net::netlayer::{NetLayer, NetMeta};
use v2xw_net::wsmp::WsmpNetLayer;

/// The engine context, as a downstream crate would supply one.
struct EngineCtx {
    now: SimTime,
    scheduler: Scheduler<&'static str>,
    rng: RngRegistry,
    provenance: ProvenanceLog,
    params: ParamSet,
    world: (),
    actors: (),
    records: Vec<(&'static str, Visibility, String)>,
}

impl EngineCtx {
    fn new() -> Self {
        Self {
            now: 0,
            scheduler: Scheduler::new(),
            rng: RngRegistry::new(7),
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            world: (),
            actors: (),
            records: Vec::new(),
        }
    }
}

impl Ctx for EngineCtx {
    type World = ();
    type Actors = ();
    type Payload = &'static str;

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
        self.records.push((
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

/// Both network layers and all four fragmenters behind trait objects, plus the model
/// handles the registry stores.
#[test]
fn every_model_works_as_a_trait_object() {
    let layers: Vec<Box<dyn NetLayer<EngineCtx>>> = vec![
        Box::new(WsmpNetLayer::default()),
        Box::new(GnBtpNetLayer::default()),
    ];
    assert_eq!(layers[0].header_bytes(&NetMeta::for_bsm(180)), 5);
    assert_eq!(layers[1].header_bytes(&NetMeta::for_cam(357)), 52);
    for l in &layers {
        assert!(!l.fragments(), "{} must not fragment", l.id());
        assert!(l.mtu() > 1_000);
    }

    let fragmenters: Vec<Box<dyn Fragmenter<EngineCtx>>> = vec![
        Box::new(NoneFragmenter::default()),
        Box::new(FacilitiesSegmentation::default()),
        Box::new(CertCyclePartialHybrid::default()),
        Box::new(GenericSduFragmenter::default()),
    ];
    for f in &fragmenters {
        assert_eq!(f.family(), Family::Fragmenter);
        f.card().validate().expect("card validates");
        // Invariant I-N2: every strategy answers for its timeout, one way or the other.
        let _ = f.reassembly_timeout();
        let m = f.loss_amplification(&[
            FragmentLoss::new(0, 0.1, 500),
            FragmentLoss::new(1, 0.1, 500),
        ]);
        assert!((m.p_any_fragment_lost - 0.19).abs() < 1e-12, "{}", f.id());
    }
    // Only the facilities strategy declines to amplify.
    let amplifies: Vec<bool> = fragmenters.iter().map(|f| f.amplifies_loss()).collect();
    assert_eq!(amplifies, vec![true, false, true, true]);

    // …and the form the registry stores them in.
    let handles: Vec<ModelHandle> = vec![
        Arc::new(WsmpNetLayer::default()),
        Arc::new(GnBtpNetLayer::default()),
        Arc::new(NoneFragmenter::default()),
        Arc::new(FacilitiesSegmentation::default()),
        Arc::new(CertCyclePartialHybrid::default()),
        Arc::new(GenericSduFragmenter::default()),
    ];
    let ids: Vec<String> = handles.iter().map(|h| h.id().to_string()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "model ids are unique");
    assert_eq!(
        ids,
        v2xw_net::MODELS
            .iter()
            .map(|(id, _)| (*id).to_string())
            .collect::<Vec<_>>(),
        "and MODELS lists them in the order they are built here"
    );
}

/// Invariant I-N1 end to end: an oversize CPM is segmented, each segment is encapsulated,
/// and every octet — payload, fragmentation overhead and network header — lands in exactly
/// one bucket once.
#[test]
fn every_byte_of_a_segmented_cpm_reaches_exactly_one_bucket() {
    let gn = GnBtpNetLayer::default();
    let frag = FacilitiesSegmentation::default();
    let mut ledger = ByteLedger::new();

    let cpm_bytes = 3_000u32;
    let segments = frag
        .split(SduId::new(1), cpm_bytes, gn.sdu_mtu())
        .expect("3 kB of CPM segments cleanly");
    assert_eq!(segments.len(), 4);

    let mut expected = 0u64;
    for seg in &segments {
        // Each segment is a complete message: payload plus the repeated containers.
        let sdu = vec![0u8; seg.total_bytes() as usize];
        let meta = NetMeta::for_cam(seg.total_bytes());
        let pdus = <GnBtpNetLayer as NetLayer<EngineCtx>>::encapsulate(&gn, &sdu, &meta)
            .expect("a segment fits the MTU by construction");
        assert_eq!(pdus.len(), 1);
        let pdu = &pdus[0];
        assert_eq!(pdu.header_bytes, 52);
        ledger.credit(BytesOnWire::new(pdu.total_bytes(), "node.tx"), Bucket::Air);
        expected += u64::from(pdu.total_bytes());
    }

    // The arithmetic, independently: the CPM's own bytes, four repetitions of the segment
    // containers, and four GeoNetworking headers.
    let by_hand =
        u64::from(cpm_bytes) + 4 * u64::from(frag.params().per_segment_overhead_bytes) + 4 * 52;
    assert_eq!(expected, by_hand);
    assert_eq!(ledger.get(Bucket::Air), by_hand);
    assert_eq!(ledger.total(), by_hand);
    assert_eq!(ledger.credited_total(), by_hand);
    assert!(ledger.is_consistent());
    // Nothing reached any other bucket.
    for bucket in Bucket::ALL {
        if bucket != Bucket::Air {
            assert_eq!(ledger.get(bucket), 0);
        }
    }
}

/// A BSM over WSMP with no fragmentation: the simplest whole path, and the one the default
/// scenario runs.
#[test]
fn a_bsm_over_wsmp_costs_its_documented_five_octets() {
    let wsmp = WsmpNetLayer::default();
    let frag = NoneFragmenter::default();
    let mut ledger = ByteLedger::new();

    // Rostami 2018's digest-signed BSM SPDU (04-models.md §9.3).
    let spdu = vec![0u8; 180];
    let parts = frag
        .split(SduId::new(1), 180, wsmp.payload_mtu())
        .expect("a BSM is far below the MTU");
    assert_eq!(parts.len(), 1);
    assert!(parts[0].is_whole());

    let pdus =
        <WsmpNetLayer as NetLayer<EngineCtx>>::encapsulate(&wsmp, &spdu, &NetMeta::for_bsm(180))
            .unwrap();
    assert_eq!(pdus[0].header_bytes, 5);
    assert_eq!(pdus[0].total_bytes(), 185);
    ledger.credit(
        BytesOnWire::new(pdus[0].total_bytes(), "node.tx"),
        Bucket::Air,
    );
    assert_eq!(ledger.total(), 185);
    assert!(ledger.is_consistent());
}

/// The generic strategy, driven through the trait from a downstream context: fragment,
/// reassemble out of order, and record.
#[test]
fn a_fragmented_sdu_reassembles_through_the_trait() {
    let mut ctx = EngineCtx::new();
    let mut frag: Box<dyn Fragmenter<EngineCtx>> = Box::new(GenericSduFragmenter::default());
    let peer = NodeId::new(11);
    let rx = NodeId::new(2);

    let parts = frag
        .fragment(SduId::new(4), 4_000, 1_398)
        .expect("4 kB splits into three fragments");
    assert_eq!(parts.len(), 3);

    // Out of order: 2, 0, 1.
    for (step, i) in [2usize, 0, 1].into_iter().enumerate() {
        ctx.now = step as u64 * 20 * NS_PER_MS;
        let out = frag.reassemble(&mut ctx, rx, &parts[i], peer);
        if step < 2 {
            assert!(matches!(out, ReassemblyOutcome::Pending { .. }));
        } else {
            assert_eq!(
                out,
                ReassemblyOutcome::Complete {
                    sdu: SduId::new(4),
                    bytes: 4_000,
                    segments: 3
                }
            );
        }
    }

    // Every record went to the NODE channel 03-interfaces.md §14 names, and none of them
    // carries the transmitter's identity.
    assert_eq!(ctx.records.len(), 3);
    for (channel, visibility, json) in &ctx.records {
        assert_eq!(*channel, "net.frag");
        assert_eq!(*visibility, Visibility::Node);
        assert!(visibility.allowed_on_node_channel());
        assert!(!json.contains("\"from\""), "{json}");
    }
}

/// The measurement hook, from the outside: the predicted loss of a certificate cycle
/// against a realised rate, with every exported float on its declared grid.
#[test]
fn the_amplification_hook_exports_quantised_figures() {
    let cert = CertCyclePartialHybrid::default();
    // At the DSRC cap the Falcon-512 certificate is one fragment, so nothing amplifies.
    assert_eq!(cert.alpha(DSRC_PAYLOAD_CAP_BYTES), Ok(1));

    // A five-fragment cycle at p = 0.1 is the case 04-models.md §7.4 works out: 41 %.
    let per_fragment: Vec<FragmentLoss> = (0..5).map(|i| FragmentLoss::new(i, 0.1, 172)).collect();
    let predicted =
        <CertCyclePartialHybrid as Fragmenter<EngineCtx>>::loss_amplification(&cert, &per_fragment)
            .p_sdu_lost();
    assert!((predicted - 0.40951).abs() < 1e-12);

    let mut meter = AmplificationMeter::new();
    for i in 0..1_000 {
        meter.observe(predicted, i % 5 == 0);
    }
    let snap = meter.snapshot();
    assert_eq!(snap.samples, 1_000);
    assert_eq!(snap.lost, 200);
    assert_eq!(snap.p_predicted, 0.41);
    assert_eq!(snap.p_realised, 0.2);
    assert!(snap.correlation_gap < 0.0, "correlated fragment loss");
    for f in [snap.p_predicted, snap.p_realised, snap.correlation_gap] {
        assert!(math::is_on_grid(f, 0.001), "{f} is off its declared grid");
    }
}
