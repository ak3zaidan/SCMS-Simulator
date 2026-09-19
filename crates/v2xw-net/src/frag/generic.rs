//! `fragmenter/generic-sdu` — split anything, reassemble with a timeout
//! (04-models.md §7.3).
//!
//! The strategy for the cases the standards do not cover: an oversize SDU is split into
//! `ceil(L / MTU_eff)` fragments, each carrying a four-byte fragment header, and the
//! receiver keeps a reassembly buffer per `(sender, SDU)` with a timeout and tolerance for
//! out-of-order arrival.
//!
//! # The four-byte header is a design choice, and it is recorded as one
//!
//! 04-models.md §7.3 specifies "a 4-byte fragment header (id 2 + index 1 + count 1, a design
//! choice recorded in the card)". No standard defines it, because no standard defines this
//! strategy: it exists so that a scenario can ask "what would it cost if we *did* fragment
//! here?". The consequences of the field widths are real, though, and are enforced:
//!
//! * the one-octet count means at most [`MAX_FRAGMENTS`] = 255 fragments, above which the
//!   split is refused with [`DropCause::TooManyFragments`];
//! * the two-octet id would alias after 65,536 outstanding SDUs from one peer. The
//!   reassembly buffer is keyed by the full [`SduId`] and is capped far below that, so the
//!   aliasing is unreachable; it is recorded as a limitation rather than modelled.
//!
//! # The timeout is a `todo-calibrate` value
//!
//! `reassembly_timeout_ms` defaults to 1,000 ms, anchored on the GeoNetworking maximum
//! packet lifetime for a CPM [TS 103 324 §5.3.3], and 04-models.md §7.3 marks it
//! `TODO: calibrate` with the plan "sweep 250-2,000 ms against the P2PCD response-time
//! anchors and keep the smallest value that does not increase failed reassemblies at
//! 60 veh/km". The card carries that plan verbatim, which is registry rule R1.
//!
//! # Loss amplification
//!
//! Every fragment is needed, so this is the strategy loss amplification is about:
//! `P_sdu = 1 - prod(1 - p_i)` over the per-fragment probabilities the PHY computed
//! (04-models.md §7.4). At `p` = 0.1, five fragments lose the SDU 41 % of the time.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::{Ctx, CtxExt};
use v2xw_core::ids::{NodeId, SduId};
use v2xw_core::model::Model;
use v2xw_core::registry::ParamSet;
use v2xw_core::time::{Duration, SimTime};

use crate::amplification;
use crate::error::DropCause;
use crate::frag::{
    FragRecord, FragmentDesc, FragmentKind, Fragmenter, ReassemblyBuffer, ReassemblyOutcome,
    split_equal,
};

/// This model's stable id.
pub const FRAGMENTER_GENERIC_ID: &str = "fragmenter/generic-sdu";

/// The fragment header: id 2 + index 1 + count 1 octets (04-models.md §7.3, a design
/// choice).
pub const FRAGMENT_HEADER_BYTES: u32 = 4;

/// The largest fragment count the one-octet count field can express.
pub const MAX_FRAGMENTS: u16 = 255;

/// The default reassembly timeout, milliseconds: the GeoNetworking maximum packet lifetime
/// for a CPM [TS 103 324 §5.3.3], marked `TODO: calibrate` by 04-models.md §7.3.
pub const DEFAULT_REASSEMBLY_TIMEOUT_MS: u64 = 1_000;

/// The default reassembly-buffer capacity, in partly received SDUs.
pub const DEFAULT_MAX_OPEN_SDUS: usize = 64;

/// The parameters of generic SDU fragmentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenericParams {
    /// The per-fragment header, bytes. Default [`FRAGMENT_HEADER_BYTES`].
    pub fragment_header_bytes: u32,
    /// How long a partly received SDU is kept, milliseconds. Default
    /// [`DEFAULT_REASSEMBLY_TIMEOUT_MS`].
    pub reassembly_timeout_ms: u64,
    /// How many partly received SDUs one node keeps at once. Default
    /// [`DEFAULT_MAX_OPEN_SDUS`].
    pub max_open_sdus: usize,
}

impl Default for GenericParams {
    fn default() -> Self {
        Self {
            fragment_header_bytes: FRAGMENT_HEADER_BYTES,
            reassembly_timeout_ms: DEFAULT_REASSEMBLY_TIMEOUT_MS,
            max_open_sdus: DEFAULT_MAX_OPEN_SDUS,
        }
    }
}

/// `fragmenter/generic-sdu`: split with a four-byte header, reassemble with a timeout.
///
/// ```
/// use v2xw_core::ids::SduId;
/// use v2xw_net::frag::generic::GenericSduFragmenter;
///
/// let f = GenericSduFragmenter::default();
/// // 4,000 B over a 1,398 B MTU: 1,394 B of content per fragment, so three fragments.
/// let parts = f.split(SduId::new(1), 4_000, 1_398).unwrap();
/// assert_eq!(parts.len(), 3);
/// assert_eq!(parts.iter().map(|p| p.payload_bytes).sum::<u32>(), 4_000);
/// assert!(parts.iter().all(|p| p.total_bytes() <= 1_398));
/// ```
#[derive(Debug, Clone)]
pub struct GenericSduFragmenter {
    params: GenericParams,
    buffer: ReassemblyBuffer,
    /// Sets retired by [`Fragmenter::reassemble`], waiting to be returned by
    /// [`Fragmenter::expire`]. Their records were emitted when they were retired; this
    /// queue is so the caller's own accounting sees each retirement exactly once.
    retired: Vec<ReassemblyOutcome>,
    card: ModelCard,
}

impl Default for GenericSduFragmenter {
    fn default() -> Self {
        Self::new(GenericParams::default())
    }
}

impl GenericSduFragmenter {
    /// The model with the given parameters.
    pub fn new(params: GenericParams) -> Self {
        Self {
            buffer: ReassemblyBuffer::new(
                params.max_open_sdus,
                Duration::from_millis(params.reassembly_timeout_ms),
            ),
            params,
            retired: Vec::new(),
            card: card(),
        }
    }

    /// The model configured from a resolved parameter set (invariant I-C3).
    pub fn from_params(params: &ParamSet) -> Self {
        let d = GenericParams::default();
        Self::new(GenericParams {
            fragment_header_bytes: params
                .get_u64("fragment_header_bytes")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(d.fragment_header_bytes),
            reassembly_timeout_ms: params
                .get_u64("reassembly_timeout_ms")
                .unwrap_or(d.reassembly_timeout_ms),
            max_open_sdus: params
                .get_u64("max_open_sdus")
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(d.max_open_sdus),
        })
    }

    /// The parameters in force.
    pub const fn params(&self) -> &GenericParams {
        &self.params
    }

    /// The SDU bytes one fragment carries at an MTU of `mtu`: the MTU less the fragment
    /// header.
    pub const fn effective_mtu(&self, mtu: u32) -> u32 {
        mtu.saturating_sub(self.params.fragment_header_bytes)
    }

    /// How many partly received SDUs this node is holding.
    pub fn open_reassemblies(&self) -> usize {
        self.buffer.open()
    }

    /// Splits an SDU — the body of [`Fragmenter::fragment`], callable without naming a
    /// context type.
    ///
    /// An SDU that already fits is passed through whole, with **no** fragment header: a
    /// receiver needs no reassembly state for it, so charging four bytes for a header it
    /// would not read would overstate the cost of the strategy on every message that never
    /// needed it.
    ///
    /// # Errors
    /// [`DropCause::Mtu`] when the fragment header leaves no room for content, and
    /// [`DropCause::TooManyFragments`] above [`MAX_FRAGMENTS`], which the one-octet count
    /// field cannot express.
    pub fn split(
        &self,
        sdu: SduId,
        sdu_bytes: u32,
        mtu: u32,
    ) -> core::result::Result<Vec<FragmentDesc>, DropCause> {
        if sdu_bytes <= mtu {
            return Ok(vec![FragmentDesc::whole(sdu, sdu_bytes)]);
        }
        let per_fragment = self.effective_mtu(mtu);
        if per_fragment == 0 {
            return Err(DropCause::Mtu { sdu_bytes, mtu });
        }
        let needed = sdu_bytes.div_ceil(per_fragment);
        if needed > u32::from(MAX_FRAGMENTS) {
            return Err(DropCause::TooManyFragments {
                needed,
                max: MAX_FRAGMENTS,
            });
        }
        let count = needed as u16;
        Ok(split_equal(sdu_bytes, count)
            .into_iter()
            .enumerate()
            .map(|(i, payload_bytes)| FragmentDesc {
                sdu,
                index: i as u16,
                count,
                header_bytes: self.params.fragment_header_bytes,
                payload_bytes,
                kind: FragmentKind::Piece,
            })
            .collect())
    }
}

impl Model for GenericSduFragmenter {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C> Fragmenter<C> for GenericSduFragmenter
where
    C: Ctx + ?Sized,
{
    fn fragment(
        &self,
        sdu: SduId,
        sdu_bytes: u32,
        mtu: u32,
    ) -> core::result::Result<Vec<FragmentDesc>, DropCause> {
        self.split(sdu, sdu_bytes, mtu)
    }

    fn reassemble(
        &mut self,
        ctx: &mut C,
        rx: NodeId,
        frag: &FragmentDesc,
        from: NodeId,
    ) -> ReassemblyOutcome {
        let now = ctx.now();
        let outcome = self.buffer.accept(now, from, frag);
        // Sets whose timer ran out before this fragment arrived are recorded as they are
        // retired and queued for the caller's own accounting (see `retired`).
        for retired in self.buffer.take_expired() {
            ctx.emit(FragRecord::new(now, rx, &retired));
            self.retired.push(retired);
        }
        if crate::frag::should_record(frag, &outcome) {
            ctx.emit(FragRecord::new(now, rx, &outcome));
        }
        outcome
    }

    fn overhead_bytes(&self) -> u32 {
        self.params.fragment_header_bytes
    }

    fn reassembly_timeout(&self) -> Option<Duration> {
        Some(Duration::from_millis(self.params.reassembly_timeout_ms))
    }

    fn expire(&mut self, ctx: &mut C, rx: NodeId, now: SimTime) -> Vec<ReassemblyOutcome> {
        let mut out = core::mem::take(&mut self.retired);
        for outcome in self.buffer.expire(now) {
            ctx.emit(FragRecord::new(now, rx, &outcome));
            out.push(outcome);
        }
        out
    }
}

/// The model card for `fragmenter/generic-sdu`.
fn card() -> ModelCard {
    let mut card = ModelCard::new(
        FRAGMENTER_GENERIC_ID,
        Family::Fragmenter,
        "1.0.0",
        "Splits any SDU into ceil(L / MTU_eff) fragments with a four-byte fragment header \
         (id 2 + index 1 + count 1, a design choice), and reassembles per (sender, SDU) with \
         a timeout and tolerance for out-of-order arrival.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "fragment count".to_string(),
            latex_or_text: "n = ceil(L / (MTU - 4)), n <= 255".to_string(),
            notes: Some(
                "Payloads are split as equally as possible and sum to exactly L. An SDU that \
                 already fits the MTU is passed through whole, with no fragment header."
                    .to_string(),
            ),
        },
        amplification::equation(),
    ];
    card.parameters = vec![
        Parameter {
            name: "fragment_header_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(FRAGMENT_HEADER_BYTES),
            range: Some(vec![serde_json::json!(0), serde_json::json!(64)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "A design choice, not a standard: no standard defines this \
                            strategy. 04-models.md §7.3 fixes the layout as id 2 + index 1 + \
                            count 1 = 4 B and requires it to be recorded on this card."
                    .to_string(),
                accessed: None,
                note: Some(
                    "The field widths are enforced rather than decorative: the one-octet \
                     count caps the split at 255 fragments."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Nothing to calibrate against: the header is invented. If the strategy is \
                 ever mapped onto a real protocol (6LoWPAN fragmentation, IPv6 fragment \
                 headers, an SAE or ETSI adaptation layer), replace the layout with that \
                 protocol's and cite it; until then the 4 B are declared as a modelling \
                 assumption so that no reader mistakes them for a standard's number."
                    .to_string(),
            ),
        },
        Parameter {
            name: "reassembly_timeout_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(DEFAULT_REASSEMBLY_TIMEOUT_MS),
            range: Some(vec![serde_json::json!(50), serde_json::json!(10_000)]),
            source: Source::todo_calibrate(
                "Anchored on the GeoNetworking maximum packet lifetime for a CPM, 1 000 ms \
                 [ETSI TS 103 324 §5.3.3], which bounds how long an oversize message's \
                 pieces are on the air — but no source gives a reassembly timeout for this \
                 invented strategy.",
            ),
            calibration: Some(
                "Sweep 250-2 000 ms against the P2PCD response-time anchors (Falcon 250 ms, \
                 XMSS 375 ms, Dilithium about 500 ms, SPHINCS+ about 1 000 ms) and keep the \
                 smallest value that does not increase failed reassemblies at 60 veh/km \
                 (04-models.md §7.3)."
                    .to_string(),
            ),
        },
        Parameter {
            name: "max_open_sdus".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(DEFAULT_MAX_OPEN_SDUS),
            range: Some(vec![serde_json::json!(1), serde_json::json!(4_096)]),
            source: Source::todo_calibrate(
                "A bounded buffer is a modelling decision: an unbounded one would make a \
                 node's memory a function of how many peers sent oversize SDUs, which is \
                 exactly the denial-of-service a real implementation caps.",
            ),
            calibration: Some(
                "Measure the number of concurrently open reassemblies at 60 veh/km with the \
                 calibrated reassembly_timeout_ms and set the cap to a value the 99th \
                 percentile does not reach; report the ReassemblyBufferFull rate as the \
                 evidence that it does not bind."
                    .to_string(),
            ),
        },
    ];
    card.assumptions = vec![
        "Reassembly is keyed per (sender, SDU). The sender is the transmitting node at \
         these tiers; at the high tier it becomes the sender's pseudonym digest, which is \
         what a receiver actually observes (04-models.md §7.3)."
            .to_string(),
        "Out-of-order arrival is normal and is tolerated: fragments are tracked as a set of \
         indices, so any arrival order reassembles identically."
            .to_string(),
        "A duplicate fragment is tolerated and its bytes are counted once, because a \
         repeated fragment is what a sender's own repetition looks like. This holds after \
         the set has completed too: a completed (sender, SDU) is remembered for one \
         reassembly timeout and a fragment arriving against it is reported `duplicate`, \
         which is not a loss, rather than opening a second reassembly that could never \
         finish."
            .to_string(),
        "The reassembly timer starts at the first fragment and is not extended, so the \
         timeout bounds the whole set's lifetime — which is the condition assumption 4 of \
         04-models.md §7.4 is about (invariant I-N2)."
            .to_string(),
        "A full buffer refuses the arriving SDU rather than evicting an older one, so the \
         loss is attributed to the SDU that could not be admitted and does not depend on an \
         eviction order."
            .to_string(),
    ];
    card.limitations = {
        let mut l = vec![
            "The fragment header is invented (see its calibration note). Nothing \
             interoperates with it."
                .to_string(),
            "The two-octet id field would alias after 65 536 outstanding SDUs from one \
             peer. The model keys its buffer by the full SduId and caps the buffer at \
             max_open_sdus, so the aliasing cannot occur here; a real 16-bit field would \
             have to handle it."
                .to_string(),
            "No per-fragment timer: one deadline covers the whole set. Per-fragment timers \
             are the high net tier (04-models.md §7.5)."
                .to_string(),
            "No retransmission of a missing fragment. There is no mechanism to request one \
             in any of the broadcast message sets this strategy would carry."
                .to_string(),
        ];
        l.extend(amplification::ASSUMPTIONS.iter().map(|s| (*s).to_string()));
        l
    };
    card.ignores = vec![
        "Per-fragment timers, P2PCD interaction and GeoNetworking forwarding, which are the \
         high net tier (04-models.md §7.5)."
            .to_string(),
        "Interleaving policy: which fragment goes out when is the sender's scheduling \
         decision, not this model's."
            .to_string(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Paper,
            "04-models.md §7.3 — 'fragmenter/generic-sdu: split any SDU into ceil(L/MTU_eff) \
             fragments with a 4-byte fragment header (id 2 + index 1 + count 1, a design \
             choice recorded in the card); reassembly buffer per (sender digest, sdu id) \
             with timeout; out-of-order tolerated'",
        ),
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 324 §5.3.3 — the 1 000 ms GeoNetworking maximum packet lifetime \
             the default timeout is anchored on",
        ),
        Source::new(SourceKind::Paper, "04-models.md §7.4 — loss amplification"),
    ];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "frag::generic::tests::fragments_reassemble_in_order_and_out_of_order".to_string(),
            "frag::generic::tests::a_partial_set_expires_after_the_timeout".to_string(),
            "frag::generic::tests::a_split_that_the_count_field_cannot_express_is_refused"
                .to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amplification::{FragmentLoss, equal_p_loss};
    use crate::testctx::TestCtx;
    use v2xw_core::time::NS_PER_MS;

    fn model() -> GenericSduFragmenter {
        GenericSduFragmenter::default()
    }

    #[test]
    fn a_split_is_exact_and_fits_the_mtu() {
        let f = model();
        let parts = f.split(SduId::new(1), 4_000, 1_398).unwrap();
        assert_eq!(parts.len(), 3, "ceil(4000 / (1398 - 4))");
        assert_eq!(parts.iter().map(|p| p.payload_bytes).sum::<u32>(), 4_000);
        for (i, p) in parts.iter().enumerate() {
            assert_eq!(p.index, i as u16);
            assert_eq!(p.count, 3);
            assert_eq!(p.kind, FragmentKind::Piece);
            assert!(!p.kind.independently_interpretable());
            assert_eq!(p.header_bytes, 4);
            assert!(p.total_bytes() <= 1_398, "fragment {i}");
        }
        assert_eq!(
            <GenericSduFragmenter as Fragmenter<TestCtx>>::overhead_bytes(&f),
            4
        );

        // An SDU that fits is passed through with no header at all.
        let whole = f.split(SduId::new(2), 1_398, 1_398).unwrap();
        assert_eq!(whole.len(), 1);
        assert!(whole[0].is_whole());
        assert_eq!(whole[0].header_bytes, 0);
    }

    /// The test 04-models.md §7.3's conformance item asks for: in-order and out-of-order
    /// reassembly.
    #[test]
    fn fragments_reassemble_in_order_and_out_of_order() {
        let peer = NodeId::new(3);
        let rx = NodeId::new(8);
        for order in [[0usize, 1, 2, 3], [3, 2, 1, 0], [2, 0, 3, 1]] {
            let mut f = model();
            let mut ctx = TestCtx::new();
            let parts = f.split(SduId::new(5), 5_000, 1_398).unwrap();
            assert_eq!(parts.len(), 4);
            let mut last = None;
            for (step, idx) in order.iter().enumerate() {
                ctx.set_now(step as u64 * 10 * NS_PER_MS);
                last = Some(f.reassemble(&mut ctx, rx, &parts[*idx], peer));
            }
            assert_eq!(
                last,
                Some(ReassemblyOutcome::Complete {
                    sdu: SduId::new(5),
                    bytes: 5_000,
                    segments: 4
                }),
                "arrival order {order:?} must not change the result"
            );
            assert_eq!(f.open_reassemblies(), 0);
            // Three Pending records and one Complete, in that order.
            let records = ctx.records_on("net.frag");
            assert_eq!(records.len(), 4);
            assert!(records[3].contains("\"outcome\":\"complete\""));
            assert!(records[3].contains("\"bytes\":5000"));
        }
    }

    #[test]
    fn a_fragment_arriving_after_the_set_completed_is_a_duplicate_and_not_a_loss() {
        // The defect this pins: `accept` removed the entry on completion, so a repeat of
        // any fragment afterwards missed the entry, fell into the "no entry" arm and
        // opened a brand-new reassembly that could never finish. A timeout later it was
        // retired as `Expired` — a reassembly loss recorded against an SDU that had been
        // delivered — and until then it held a buffer slot.
        let mut f = model();
        let mut ctx = TestCtx::new();
        let peer = NodeId::new(3);
        let rx = NodeId::new(8);
        let parts = f.split(SduId::new(6), 4_000, 1_398).unwrap();
        for part in &parts {
            f.reassemble(&mut ctx, rx, part, peer);
        }
        assert_eq!(f.open_reassemblies(), 0, "the set completed");

        // The sender repeats fragment 0 ten milliseconds later.
        ctx.set_now(10 * NS_PER_MS);
        let out = f.reassemble(&mut ctx, rx, &parts[0], peer);
        assert_eq!(
            out,
            ReassemblyOutcome::Duplicate {
                sdu: SduId::new(6),
                segments: 3,
            }
        );
        assert!(!out.is_loss(), "a repeat of a delivered SDU is not a loss");
        assert_eq!(f.open_reassemblies(), 0, "and it opened no reassembly");
        // Every fragment of it, repeated, and in any order: still no state.
        for index in [2usize, 1, 0, 2] {
            assert!(matches!(
                f.reassemble(&mut ctx, rx, &parts[index], peer),
                ReassemblyOutcome::Duplicate { segments: 3, .. }
            ));
        }
        assert_eq!(f.open_reassemblies(), 0);

        // Nothing is ever retired, at any later instant.
        ctx.set_now(5_000 * NS_PER_MS);
        assert!(f.expire(&mut ctx, rx, 5_000 * NS_PER_MS).is_empty());
        let loss_records = ctx
            .records_on("net.frag")
            .iter()
            .filter(|j| {
                j.contains("\"outcome\":\"expired\"") || j.contains("\"outcome\":\"failed\"")
            })
            .count();
        assert_eq!(loss_records, 0, "a delivered SDU produced a loss record");
        // The duplicates are on the channel, labelled as what they are, carrying no bytes
        // — the payload was accounted for when the set completed.
        let duplicates: Vec<&str> = ctx
            .records_on("net.frag")
            .iter()
            .copied()
            .filter(|j| j.contains("\"outcome\":\"duplicate\""))
            .collect();
        assert_eq!(duplicates.len(), 5);
        assert!(duplicates.iter().all(|j| j.contains("\"bytes\":0")));
    }

    #[test]
    fn a_remembered_completion_holds_no_reassembly_slot_and_is_released_on_time() {
        // The second half of the same defect: the repeats consumed buffer slots, so a
        // genuinely new SDU was refused with `ReassemblyBufferFull` behind two SDUs that
        // had already been delivered. The memory of a completion must cost no slot, and
        // must not last for ever either.
        let mut f = GenericSduFragmenter::new(GenericParams {
            max_open_sdus: 2,
            ..GenericParams::default()
        });
        let mut ctx = TestCtx::new();
        let rx = NodeId::new(8);
        let peers = [NodeId::new(1), NodeId::new(2)];
        let mut all: Vec<Vec<FragmentDesc>> = Vec::new();
        for (i, peer) in peers.iter().enumerate() {
            let parts = f.split(SduId::new(10 + i as u32), 4_000, 1_398).unwrap();
            for part in &parts {
                f.reassemble(&mut ctx, rx, part, *peer);
            }
            all.push(parts);
        }
        assert_eq!(f.open_reassemblies(), 0);
        // Both peers repeat a fragment of their delivered SDU.
        ctx.set_now(10 * NS_PER_MS);
        for (parts, peer) in all.iter().zip(peers) {
            assert!(matches!(
                f.reassemble(&mut ctx, rx, &parts[0], peer),
                ReassemblyOutcome::Duplicate { .. }
            ));
        }
        assert_eq!(f.open_reassemblies(), 0, "duplicates took reassembly slots");

        // A third, genuinely new SDU is admitted — the buffer is empty.
        let fresh = f.split(SduId::new(99), 4_000, 1_398).unwrap();
        let out = f.reassemble(&mut ctx, rx, &fresh[0], NodeId::new(7));
        assert!(
            matches!(
                out,
                ReassemblyOutcome::Pending {
                    have: 1,
                    want: 3,
                    ..
                }
            ),
            "a new SDU was refused behind two delivered ones: {out:?}"
        );

        // And the memory is released one timeout after the completion it records, not
        // later: at t = 1,000 ms the repeats are ordinary first fragments again.
        ctx.set_now(1_000 * NS_PER_MS);
        f.expire(&mut ctx, rx, 1_000 * NS_PER_MS);
        let out = f.reassemble(&mut ctx, rx, &all[0][0], peers[0]);
        assert!(
            matches!(
                out,
                ReassemblyOutcome::Pending {
                    have: 1,
                    want: 3,
                    ..
                }
            ),
            "the completion was remembered past its timeout: {out:?}"
        );
    }

    #[test]
    fn a_repeat_claiming_a_different_fragment_count_is_still_refused() {
        // The open-set rule, applied to a completed one: a repeat that disagrees about how
        // many fragments the SDU had is malformed however late it is, and is reported the
        // same way rather than quietly accepted as a duplicate.
        let mut f = model();
        let mut ctx = TestCtx::new();
        let peer = NodeId::new(3);
        let rx = NodeId::new(8);
        let parts = f.split(SduId::new(6), 4_000, 1_398).unwrap();
        for part in &parts {
            f.reassemble(&mut ctx, rx, part, peer);
        }
        let mut liar = parts[0];
        liar.count = 4;
        ctx.set_now(10 * NS_PER_MS);
        assert_eq!(
            f.reassemble(&mut ctx, rx, &liar, peer),
            ReassemblyOutcome::Failed {
                sdu: SduId::new(6),
                cause: DropCause::FragmentCountMismatch { had: 3, saw: 4 },
            }
        );
    }

    #[test]
    fn a_partial_set_expires_after_the_timeout() {
        let mut f = model();
        let mut ctx = TestCtx::new();
        let peer = NodeId::new(3);
        let rx = NodeId::new(8);
        assert_eq!(
            <GenericSduFragmenter as Fragmenter<TestCtx>>::reassembly_timeout(&f),
            Some(Duration::from_millis(1_000))
        );

        let parts = f.split(SduId::new(6), 4_000, 1_398).unwrap();
        f.reassemble(&mut ctx, rx, &parts[0], peer);
        f.reassemble(&mut ctx, rx, &parts[1], peer);

        // Just before the deadline, nothing is retired.
        ctx.set_now(1_000 * NS_PER_MS - 1);
        assert!(f.expire(&mut ctx, rx, 1_000 * NS_PER_MS - 1).is_empty());
        assert_eq!(f.open_reassemblies(), 1);

        // At the deadline the set is retired, with what had arrived.
        ctx.set_now(1_000 * NS_PER_MS);
        let retired = f.expire(&mut ctx, rx, 1_000 * NS_PER_MS);
        assert_eq!(
            retired,
            vec![ReassemblyOutcome::Expired {
                sdu: SduId::new(6),
                have: 2,
                want: 3,
                bytes: 2_667,
            }]
        );
        assert_eq!(f.open_reassemblies(), 0);
        assert!(
            ctx.records_on("net.frag")
                .last()
                .unwrap()
                .contains("\"outcome\":\"expired\"")
        );
        // Retiring is idempotent: the set is gone.
        assert!(f.expire(&mut ctx, rx, 2_000 * NS_PER_MS).is_empty());
    }

    /// A set retired while a *later* fragment is being accepted is reported exactly once,
    /// both as a record and to the caller.
    #[test]
    fn a_set_retired_during_reassembly_is_reported_once() {
        let mut f = model();
        let mut ctx = TestCtx::new();
        let peer = NodeId::new(3);
        let rx = NodeId::new(8);
        let parts = f.split(SduId::new(7), 4_000, 1_398).unwrap();
        f.reassemble(&mut ctx, rx, &parts[0], peer);

        // A fragment 1.5 s later: the old set is stale, and this one opens a new set.
        ctx.set_now(1_500 * NS_PER_MS);
        let out = f.reassemble(&mut ctx, rx, &parts[1], peer);
        assert!(matches!(
            out,
            ReassemblyOutcome::Pending {
                have: 1,
                want: 3,
                ..
            }
        ));

        let expired_records = ctx
            .records_on("net.frag")
            .iter()
            .filter(|j| j.contains("\"outcome\":\"expired\""))
            .count();
        assert_eq!(expired_records, 1);

        let retired = f.expire(&mut ctx, rx, 1_500 * NS_PER_MS);
        assert_eq!(retired.len(), 1, "returned to the caller exactly once");
        assert!(matches!(
            retired[0],
            ReassemblyOutcome::Expired { have: 1, .. }
        ));
        assert_eq!(
            ctx.records_on("net.frag")
                .iter()
                .filter(|j| j.contains("\"outcome\":\"expired\""))
                .count(),
            1,
            "and recorded exactly once"
        );
    }

    #[test]
    fn a_split_that_the_count_field_cannot_express_is_refused() {
        let f = model();
        // 255 fragments of 96 B fit; 256 do not.
        assert_eq!(f.split(SduId::new(1), 24_480, 100).unwrap().len(), 255);
        assert_eq!(
            f.split(SduId::new(1), 24_481, 100),
            Err(DropCause::TooManyFragments {
                needed: 256,
                max: 255
            })
        );
        // An MTU the header alone fills leaves no room for content.
        assert_eq!(
            f.split(SduId::new(1), 100, 4),
            Err(DropCause::Mtu {
                sdu_bytes: 100,
                mtu: 4
            })
        );
    }

    #[test]
    fn a_full_buffer_refuses_new_sdus() {
        let mut f = GenericSduFragmenter::new(GenericParams {
            max_open_sdus: 2,
            ..GenericParams::default()
        });
        let mut ctx = TestCtx::new();
        let peer = NodeId::new(3);
        let rx = NodeId::new(8);
        for id in 1..=2u32 {
            let parts = f.split(SduId::new(id), 4_000, 1_398).unwrap();
            f.reassemble(&mut ctx, rx, &parts[0], peer);
        }
        let parts = f.split(SduId::new(3), 4_000, 1_398).unwrap();
        assert_eq!(
            f.reassemble(&mut ctx, rx, &parts[0], peer),
            ReassemblyOutcome::Failed {
                sdu: SduId::new(3),
                cause: DropCause::ReassemblyBufferFull { open: 2, max: 2 }
            }
        );
    }

    /// The amplification this strategy is the reason for.
    #[test]
    fn loss_amplifies_over_the_fragments() {
        let f = model();
        let five: Vec<FragmentLoss> = (0..5).map(|i| FragmentLoss::new(i, 0.1, 800)).collect();
        let m = <GenericSduFragmenter as Fragmenter<TestCtx>>::loss_amplification(&f, &five);
        assert!(m.amplifies);
        assert!((m.p_sdu_lost() - equal_p_loss(5, 0.1)).abs() < 1e-15);
        assert!((m.p_sdu_lost() - 0.40951).abs() < 1e-12);
        assert!((m.amplification_factor(0.1) - 4.0951).abs() < 1e-12);

        // Unequal per-fragment probabilities, which is the case the model must use: the
        // last fragment is shorter and therefore less likely to be lost.
        let uneven = [
            FragmentLoss::new(0, 0.12, 1_394),
            FragmentLoss::new(1, 0.12, 1_394),
            FragmentLoss::new(2, 0.04, 212),
        ];
        let m = <GenericSduFragmenter as Fragmenter<TestCtx>>::loss_amplification(&f, &uneven);
        // 1 - 0.88 * 0.88 * 0.96 = 0.256576
        assert!((m.p_sdu_lost() - 0.256_576).abs() < 1e-12, "{m:?}");
    }

    /// The measurement hook of §7.4, wired to the model: predicted against realised.
    #[test]
    fn the_amplification_hook_exposes_correlated_loss() {
        let f = model();
        let mut meter = amplification::AmplificationMeter::new();
        let per_fragment: Vec<FragmentLoss> =
            (0..3).map(|i| FragmentLoss::new(i, 0.2, 1_394)).collect();
        let predicted =
            <GenericSduFragmenter as Fragmenter<TestCtx>>::loss_amplification(&f, &per_fragment)
                .p_sdu_lost();
        assert!((predicted - 0.488).abs() < 1e-12);

        // The realised loss is only 25 %: the three fragments shared a fading state.
        for i in 0..100 {
            meter.observe(predicted, i % 4 == 0);
        }
        assert!((meter.predicted_mean() - 0.488).abs() < 1e-12);
        assert!((meter.realised() - 0.25).abs() < 1e-12);
        assert!(meter.correlation_gap() < 0.0);
        assert_eq!(meter.snapshot().p_predicted, 0.488);
    }

    #[test]
    fn the_card_validates_and_every_uncited_default_has_a_plan() {
        let f = model();
        let card = f.card();
        card.validate().expect("card validates (registry rule R1)");
        card.check_api_version().expect("api version matches");
        assert_eq!(card.family, Family::Fragmenter);
        assert!(!card.determinism.uses_rng);

        let todo: Vec<&str> = card.todo_calibrate().map(|p| p.name.as_str()).collect();
        assert_eq!(
            todo,
            vec![
                "fragment_header_bytes",
                "reassembly_timeout_ms",
                "max_open_sdus"
            ],
            "every number without a standard behind it is declared as such"
        );
        assert!(
            card.parameters
                .iter()
                .find(|p| p.name == "reassembly_timeout_ms")
                .and_then(|p| p.calibration.as_ref())
                .is_some_and(|c| c.contains("250-2 000 ms") && c.contains("60 veh/km")),
            "the documented calibration plan is on the card verbatim"
        );
        for a in amplification::ASSUMPTIONS {
            assert!(card.limitations.iter().any(|l| l == a));
        }
    }

    #[test]
    fn parameters_come_from_the_resolved_set() {
        let card = card();
        let params = ParamSet::resolve(
            &card,
            &serde_json::json!({"reassembly_timeout_ms": 250, "max_open_sdus": 8}),
        )
        .expect("both overrides are declared and in range");
        let f = GenericSduFragmenter::from_params(&params);
        assert_eq!(f.params().reassembly_timeout_ms, 250);
        assert_eq!(f.params().max_open_sdus, 8);
        assert_eq!(
            <GenericSduFragmenter as Fragmenter<TestCtx>>::reassembly_timeout(&f),
            Some(Duration::from_millis(250))
        );
    }
}
