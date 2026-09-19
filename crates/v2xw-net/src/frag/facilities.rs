//! `fragmenter/facilities-segmentation` — ETSI facilities-layer segmentation
//! (04-models.md §7.3).
//!
//! GeoNetworking has no fragmentation field, so ETSI puts the split one layer up, in the
//! service that produces the message:
//!
//! * **MAPEM.** The RLT service fragments when the message exceeds the allowed length, and
//!   the fragments are identified by `layerID` [TS 103 301 §6.4.1; ISO/TS 19091 Annex G].
//! * **CPM.** The CPS assembles *independently interpretable* segments carrying
//!   `messageSegmentInfo` when more than one CPM is generated for an event
//!   [TS 103 324 §6.1.2.1, §7.1.x].
//!
//! # Why this strategy does not amplify loss
//!
//! Each segment is a complete, separately signed message. Losing one loses **its own
//! objects and nothing else**: the others decode, verify and are used. That is the single
//! property that distinguishes this model from [`crate::frag::generic`], and it is why
//! [`Fragmenter::amplifies_loss`] is `false` here and why
//! [`crate::amplification::SduLossModel::expected_content_lost`] — the payload-weighted mean
//! of the per-segment losses — is the figure that matters rather than
//! `1 - prod(1 - p_i)`.
//!
//! It also means there is **no reassembly buffer and no timeout**: a receiver delivers each
//! segment as it arrives. [`Fragmenter::reassembly_timeout`] is therefore `None`, and the
//! related bound is the GeoNetworking maximum packet lifetime for a CPM, 1,000 ms
//! [TS 103 324 §5.3.3], which [`FacilitiesSegmentation::max_packet_lifetime`] exposes for a
//! metric provider that wants to know how long an event's segments may be spread over.
//!
//! # The segment size
//!
//! TS 103 324 §6.1.3.1 gives the CPM limit as `MTU_CPM = MTU_AL − HD_CPM − HD_NT`: the
//! access-layer MTU less the CPM's own and the networking/transport headers. The caller
//! passes that as the `mtu` argument — [`crate::gn::GnBtpNetLayer::sdu_mtu`] is
//! `MTU_AL − GN_MAX`, and the security envelope's bytes are `v2xw-sec`'s. On top of it this
//! model applies the ETSI simulation segmentation threshold of 1,100 B [TR 103 562 §5.5],
//! which is lower than either network layer's MTU and is therefore what actually triggers
//! segmentation in a default scenario.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::{Ctx, CtxExt};
use v2xw_core::ids::{NodeId, SduId};
use v2xw_core::model::Model;
use v2xw_core::registry::ParamSet;
use v2xw_core::time::Duration;

use crate::amplification;
use crate::error::DropCause;
use crate::frag::{
    FragRecord, FragmentDesc, FragmentKind, Fragmenter, ReassemblyOutcome, split_equal,
};

/// This model's stable id.
pub const FRAGMENTER_FACILITIES_ID: &str = "fragmenter/facilities-segmentation";

/// The ETSI simulation segmentation threshold, bytes [TR 103 562 §5.5, VERIFIED].
pub const ETSI_SEGMENTATION_THRESHOLD_BYTES: u32 = 1_100;

/// The GeoNetworking maximum packet lifetime for a CPM, milliseconds
/// [TS 103 324 §5.3.3, VERIFIED].
pub const CPM_MAX_PACKET_LIFETIME_MS: u64 = 1_000;

/// The repeated mandatory CPM containers — header, management and station data — bytes
/// [TR 103 562 Table 3 via 04-models.md §8.2].
///
/// The default for [`FacilitiesParams::per_segment_overhead_bytes`]; see that field for why
/// it is a `todo-calibrate` value despite having a cited anchor.
pub const CPM_REPEATED_CONTAINERS_BYTES: u32 = 121;

/// The parameters of facilities-layer segmentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacilitiesParams {
    /// The size at which the facilities layer starts segmenting, bytes. Default
    /// [`ETSI_SEGMENTATION_THRESHOLD_BYTES`]; the effective trigger is the smaller of this
    /// and the MTU the caller passes.
    pub segmentation_threshold_bytes: u32,
    /// What each segment repeats: its own message header, management container and station
    /// data, plus `messageSegmentInfo`, bytes.
    pub per_segment_overhead_bytes: u32,
    /// The GeoNetworking maximum packet lifetime for the segmented message, milliseconds.
    pub max_packet_lifetime_ms: u64,
}

impl Default for FacilitiesParams {
    fn default() -> Self {
        Self {
            segmentation_threshold_bytes: ETSI_SEGMENTATION_THRESHOLD_BYTES,
            per_segment_overhead_bytes: CPM_REPEATED_CONTAINERS_BYTES,
            max_packet_lifetime_ms: CPM_MAX_PACKET_LIFETIME_MS,
        }
    }
}

/// `fragmenter/facilities-segmentation`: independently interpretable segments.
///
/// ```
/// use v2xw_core::ids::SduId;
/// use v2xw_net::frag::facilities::FacilitiesSegmentation;
///
/// let f = FacilitiesSegmentation::default();
/// // A 2,000 B CPM under GeoNetworking's 1,398 B MTU: the 1,100 B ETSI threshold triggers
/// // first, and each segment repeats 121 B of containers.
/// let segments = f.split(SduId::new(1), 2_000, 1_398).unwrap();
/// assert_eq!(segments.len(), 3);
/// assert_eq!(segments.iter().map(|s| s.payload_bytes).sum::<u32>(), 2_000);
/// assert!(segments.iter().all(|s| s.kind.independently_interpretable()));
/// ```
#[derive(Debug, Clone)]
pub struct FacilitiesSegmentation {
    params: FacilitiesParams,
    card: ModelCard,
}

impl Default for FacilitiesSegmentation {
    fn default() -> Self {
        Self::new(FacilitiesParams::default())
    }
}

impl FacilitiesSegmentation {
    /// The model with the given parameters.
    pub fn new(params: FacilitiesParams) -> Self {
        Self {
            params,
            card: card(),
        }
    }

    /// The model configured from a resolved parameter set (invariant I-C3).
    pub fn from_params(params: &ParamSet) -> Self {
        let d = FacilitiesParams::default();
        let read_u32 = |name: &str, fallback: u32| {
            params
                .get_u64(name)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback)
        };
        Self::new(FacilitiesParams {
            segmentation_threshold_bytes: read_u32(
                "segmentation_threshold_bytes",
                d.segmentation_threshold_bytes,
            ),
            per_segment_overhead_bytes: read_u32(
                "per_segment_overhead_bytes",
                d.per_segment_overhead_bytes,
            ),
            max_packet_lifetime_ms: params
                .get_u64("max_packet_lifetime_ms")
                .unwrap_or(d.max_packet_lifetime_ms),
        })
    }

    /// The parameters in force.
    pub const fn params(&self) -> &FacilitiesParams {
        &self.params
    }

    /// The GeoNetworking maximum packet lifetime for the segmented message.
    ///
    /// Not a reassembly timeout — there is no reassembly — but the window within which an
    /// event's segments are on the air [TS 103 324 §5.3.3].
    pub const fn max_packet_lifetime(&self) -> Duration {
        Duration::from_millis(self.params.max_packet_lifetime_ms)
    }

    /// The size at which segmentation actually starts for a link of MTU `mtu`: the smaller
    /// of the configured threshold and the MTU.
    pub const fn effective_threshold(&self, mtu: u32) -> u32 {
        if self.params.segmentation_threshold_bytes < mtu {
            self.params.segmentation_threshold_bytes
        } else {
            mtu
        }
    }

    /// Segments an SDU — the body of [`Fragmenter::fragment`], callable without naming a
    /// context type.
    ///
    /// Each segment carries [`FacilitiesParams::per_segment_overhead_bytes`] of repeated
    /// containers, so the content per segment is the effective threshold less that
    /// overhead, and the segments' payloads sum to exactly `sdu_bytes`.
    ///
    /// # Errors
    /// [`DropCause::Mtu`] when the repeated containers leave no room for content, and
    /// [`DropCause::TooManyFragments`] when the split would need more segments than a
    /// `u16` count can express.
    pub fn split(
        &self,
        sdu: SduId,
        sdu_bytes: u32,
        mtu: u32,
    ) -> core::result::Result<Vec<FragmentDesc>, DropCause> {
        let threshold = self.effective_threshold(mtu);
        if sdu_bytes <= threshold {
            // Below the threshold the service emits one ordinary message; no segment
            // containers are repeated, so there is no overhead to charge.
            return Ok(vec![FragmentDesc::whole(sdu, sdu_bytes)]);
        }
        let per_segment = threshold.saturating_sub(self.params.per_segment_overhead_bytes);
        if per_segment == 0 {
            return Err(DropCause::Mtu {
                sdu_bytes,
                mtu: threshold,
            });
        }
        let needed = sdu_bytes.div_ceil(per_segment);
        let count = u16::try_from(needed).map_err(|_| DropCause::TooManyFragments {
            needed,
            max: u16::MAX,
        })?;
        Ok(split_equal(sdu_bytes, count)
            .into_iter()
            .enumerate()
            .map(|(i, payload_bytes)| FragmentDesc {
                sdu,
                index: i as u16,
                count,
                header_bytes: self.params.per_segment_overhead_bytes,
                payload_bytes,
                kind: FragmentKind::Segment,
            })
            .collect())
    }
}

impl Model for FacilitiesSegmentation {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C> Fragmenter<C> for FacilitiesSegmentation
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
        _from: NodeId,
    ) -> ReassemblyOutcome {
        // Each segment is a complete, separately signed message: it is delivered on its own,
        // whatever happened to its siblings. That is the model.
        let outcome = ReassemblyOutcome::Complete {
            sdu: frag.sdu,
            bytes: frag.payload_bytes,
            segments: 1,
        };
        if crate::frag::should_record(frag, &outcome) {
            ctx.emit(FragRecord::new(ctx.now(), rx, &outcome));
        }
        outcome
    }

    fn overhead_bytes(&self) -> u32 {
        self.params.per_segment_overhead_bytes
    }

    fn reassembly_timeout(&self) -> Option<Duration> {
        // No reassembly state exists, so nothing can time out (invariant I-N2: the card
        // says so in words, this says so in code).
        None
    }

    fn amplifies_loss(&self) -> bool {
        false
    }
}

/// The model card for `fragmenter/facilities-segmentation`.
fn card() -> ModelCard {
    let mut card = ModelCard::new(
        FRAGMENTER_FACILITIES_ID,
        Family::Fragmenter,
        "1.0.0",
        "ETSI facilities-layer segmentation (MAPEM layerID, CPM messageSegmentInfo): the \
         service splits an oversize message into independently interpretable segments, each \
         a complete separately signed message, so losing one segment loses only its own \
         content.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        v2xw_core::card::Equation {
            name: "segment count".to_string(),
            latex_or_text: "n = ceil(L / (min(threshold, MTU) - overhead))".to_string(),
            notes: Some(
                "MTU_CPM = MTU_AL - HD_CPM - HD_NT [TS 103 324 §6.1.3.1] is what the caller \
                 passes as MTU; the threshold is the ETSI simulation value of 1,100 B \
                 [TR 103 562 §5.5]. Payloads are split as equally as possible and sum to \
                 exactly L."
                    .to_string(),
            ),
        },
        amplification::equation(),
    ];
    card.parameters = vec![
        Parameter {
            name: "segmentation_threshold_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(ETSI_SEGMENTATION_THRESHOLD_BYTES),
            range: Some(vec![serde_json::json!(200), serde_json::json!(2_304)]),
            source: Source {
                kind: SourceKind::Standard,
                reference: "ETSI TR 103 562 §5.5 — segmentation threshold used in the CPM \
                            simulation study"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: Some(
                    "VERIFIED. The normative trigger is TS 103 324 §6.1.3.1's MTU_CPM; this \
                     threshold is lower and therefore the one that fires first."
                        .to_string(),
                ),
            },
            calibration: None,
        },
        Parameter {
            name: "per_segment_overhead_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(CPM_REPEATED_CONTAINERS_BYTES),
            range: Some(vec![serde_json::json!(0), serde_json::json!(512)]),
            source: Source::todo_calibrate(
                "The 121 B anchor is TR 103 562 Table 3's mandatory CPM DEs (header + \
                 management + station data), which is what each segment repeats; the \
                 messageSegmentInfo DE itself is not separately sized in any accessible \
                 source, so 121 B is a floor rather than a measured overhead.",
            ),
            calibration: Some(
                "Encode a two-segment CPM with the real TS 103 324 module once it is \
                 imported (build decision D2) and measure the repeated header, management \
                 and station-data containers plus messageSegmentInfo; replace the default \
                 with the measured value and record the anchor spread on this card."
                    .to_string(),
            ),
        },
        Parameter {
            name: "max_packet_lifetime_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(CPM_MAX_PACKET_LIFETIME_MS),
            range: Some(vec![serde_json::json!(100), serde_json::json!(10_000)]),
            source: Source {
                kind: SourceKind::Standard,
                reference: "ETSI TS 103 324 §5.3.3 — GeoNetworking maximum packet lifetime \
                            for a CPM is at most 1 000 ms"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: None,
            },
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "Every segment is a complete, separately signed message, so a receiver interprets \
         each one on its own: there is no reassembly, no reassembly buffer and no \
         reassembly timeout (invariant I-N2: the timeout is 'none')."
            .to_string(),
        "Content is divided as equally as possible across the segments. A real CPS divides \
         by perceived object, so segment sizes vary with how many objects each carries; the \
         equal split is the size-accurate stand-in."
            .to_string(),
        "The caller passes MTU_CPM (MTU_AL less the CPM's own and the networking/transport \
         headers) as the MTU; this model does not compute it."
            .to_string(),
    ];
    card.limitations = {
        let mut l = vec![
            "Loss does not amplify here, so the SDU-level figure this model reports is the \
             payload-weighted mean of the per-segment losses, not 1 - prod(1 - p_i). The \
             product is still reported as SduLossModel::p_any_fragment_lost — the chance \
             that some content is missing — because that is the quantity a completeness \
             metric needs."
                .to_string(),
            "No segment-count cap is enforced. TS 103 324's messageSegmentInfo constrains \
             the number of segments, but the constraint was not extracted from an \
             accessible source, so a configuration that produces an implausible number of \
             segments is not refused (it is refused only above a u16 count)."
                .to_string(),
            "MAPEM's layerID fragmentation and CPM's messageSegmentInfo segmentation are \
             modelled by one size rule. The identifiers differ and so do the services; what \
             they share — independently interpretable pieces, no reassembly — is what this \
             model implements."
                .to_string(),
        ];
        l.extend(amplification::ASSUMPTIONS.iter().map(|s| (*s).to_string()));
        l
    };
    card.ignores = vec![
        "Object selection: which perceived objects go into which segment is the CPM \
         generator's rule (04-models.md §8.1), not this model's."
            .to_string(),
        "The per-segment security envelope's own bytes, which are v2xw-sec's \
         (04-models.md §9.1) and reach this model inside the SDU size."
            .to_string(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 301 §6.4.1 — RLT service fragmentation of MAPEM by layerID \
             (ISO/TS 19091 Annex G)",
        ),
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 324 V2.1.1 §6.1.2.1, §6.1.3.1, §5.3.3, §7.1 — CPM segmentation, \
             MTU_CPM and the maximum packet lifetime",
        ),
        Source::new(
            SourceKind::Standard,
            "ETSI TR 103 562 §5.5, Table 3 — the simulation segmentation threshold and the \
             mandatory CPM containers",
        ),
        Source::new(SourceKind::Paper, "04-models.md §7.3, §7.4, §8.2"),
    ];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "frag::facilities::tests::an_oversize_message_becomes_independent_segments".to_string(),
            "frag::facilities::tests::losing_one_segment_loses_only_its_content".to_string(),
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
    use crate::amplification::FragmentLoss;
    use crate::testctx::TestCtx;

    #[test]
    fn an_oversize_message_becomes_independent_segments() {
        let f = FacilitiesSegmentation::default();
        // 2,000 B of CPM content: threshold 1,100 less 121 B of repeated containers gives
        // 979 B per segment, so three segments.
        let segments = f.split(SduId::new(1), 2_000, 1_398).unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments.iter().map(|s| s.payload_bytes).sum::<u32>(),
            2_000,
            "the split loses no content"
        );
        for (i, s) in segments.iter().enumerate() {
            assert_eq!(s.index, i as u16);
            assert_eq!(s.count, 3);
            assert_eq!(s.kind, FragmentKind::Segment);
            assert!(s.kind.independently_interpretable());
            assert_eq!(s.header_bytes, 121);
            assert!(s.total_bytes() <= 1_100, "each segment fits the threshold");
        }
        // Payloads are within one byte of each other.
        assert_eq!(
            segments.iter().map(|s| s.payload_bytes).collect::<Vec<_>>(),
            vec![667, 667, 666]
        );
    }

    #[test]
    fn a_message_below_the_threshold_is_not_segmented() {
        let f = FacilitiesSegmentation::default();
        for bytes in [0u32, 121, 1_000, 1_100] {
            let out = f.split(SduId::new(1), bytes, 1_398).unwrap();
            assert_eq!(out.len(), 1, "{bytes} B");
            assert!(out[0].is_whole());
            assert_eq!(out[0].header_bytes, 0, "nothing is repeated");
            assert_eq!(out[0].kind, FragmentKind::Whole);
        }
        assert_eq!(f.split(SduId::new(1), 1_101, 1_398).unwrap().len(), 2);
    }

    /// A tighter MTU than the threshold takes over as the trigger.
    #[test]
    fn the_mtu_overrides_a_larger_threshold() {
        let f = FacilitiesSegmentation::new(FacilitiesParams {
            segmentation_threshold_bytes: 2_000,
            ..FacilitiesParams::default()
        });
        assert_eq!(f.effective_threshold(1_398), 1_398);
        let segments = f.split(SduId::new(1), 2_000, 1_398).unwrap();
        // 1,398 - 121 = 1,277 B of content per segment.
        assert_eq!(segments.len(), 2);
        assert_eq!(segments.iter().map(|s| s.payload_bytes).sum::<u32>(), 2_000);
    }

    /// The headline property: a lost segment costs only its own objects.
    #[test]
    fn losing_one_segment_loses_only_its_content() {
        let f = FacilitiesSegmentation::default();
        let losses = [
            FragmentLoss::new(0, 0.10, 667),
            FragmentLoss::new(1, 0.10, 667),
            FragmentLoss::new(2, 0.10, 666),
        ];
        let m = <FacilitiesSegmentation as Fragmenter<TestCtx>>::loss_amplification(&f, &losses);
        assert!(!m.amplifies);
        // The chance that *something* is missing is the amplified figure…
        assert!((m.p_any_fragment_lost - 0.271).abs() < 1e-12);
        // …but the expected content loss is just the per-segment rate.
        assert!((m.p_sdu_lost() - 0.10).abs() < 1e-12);
        assert!((m.amplification_factor(0.10) - 1.0).abs() < 1e-12);

        // …and at the receiver each segment is delivered on its own.
        let mut f = f;
        let mut ctx = TestCtx::new();
        let segments = f.split(SduId::new(7), 2_000, 1_398).unwrap();
        let delivered: u32 = segments
            .iter()
            .skip(1) // segment 0 was lost on the air
            .map(
                |s| match f.reassemble(&mut ctx, NodeId::new(2), s, NodeId::new(5)) {
                    ReassemblyOutcome::Complete { bytes, .. } => bytes,
                    other => panic!("a segment is always deliverable, got {other:?}"),
                },
            )
            .sum();
        assert_eq!(delivered, 1_333, "two of three segments' content arrived");
        assert_eq!(ctx.records_on("net.frag").len(), 2);
    }

    #[test]
    fn an_overhead_that_eats_the_whole_threshold_is_refused() {
        let f = FacilitiesSegmentation::new(FacilitiesParams {
            segmentation_threshold_bytes: 100,
            per_segment_overhead_bytes: 121,
            ..FacilitiesParams::default()
        });
        assert_eq!(
            f.split(SduId::new(1), 500, 1_398),
            Err(DropCause::Mtu {
                sdu_bytes: 500,
                mtu: 100
            })
        );
    }

    #[test]
    fn there_is_no_reassembly_timeout_but_there_is_a_packet_lifetime() {
        let f = FacilitiesSegmentation::default();
        assert_eq!(
            <FacilitiesSegmentation as Fragmenter<TestCtx>>::reassembly_timeout(&f),
            None,
            "independently interpretable segments need no reassembly"
        );
        assert_eq!(f.max_packet_lifetime(), Duration::from_millis(1_000));
        assert_eq!(
            <FacilitiesSegmentation as Fragmenter<TestCtx>>::overhead_bytes(&f),
            121
        );
    }

    #[test]
    fn the_card_validates_and_its_uncited_default_has_a_plan() {
        let f = FacilitiesSegmentation::default();
        let card = f.card();
        card.validate().expect("card validates (registry rule R1)");
        card.check_api_version().expect("api version matches");
        assert_eq!(card.family, Family::Fragmenter);
        assert!(!card.determinism.uses_rng);

        let todo: Vec<_> = card.todo_calibrate().collect();
        assert_eq!(todo.len(), 1);
        assert_eq!(todo[0].name, "per_segment_overhead_bytes");
        assert!(
            todo[0]
                .calibration
                .as_ref()
                .is_some_and(|c| c.contains("measure")),
            "rule R1: a todo-calibrate default carries a plan"
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
            &serde_json::json!({"segmentation_threshold_bytes": 1_400}),
        )
        .expect("the override is declared and in range");
        let f = FacilitiesSegmentation::from_params(&params);
        assert_eq!(f.params().segmentation_threshold_bytes, 1_400);
        assert_eq!(f.params().per_segment_overhead_bytes, 121);
    }
}
