//! `fragmenter/none` — refuse an oversize SDU (04-models.md §7.3).
//!
//! The default for BSM and CAM, and the only strategy that is *correct* for them: a BSM is
//! 180-250 B and a secured CAM 182-807 B (04-models.md §8.2, §9.3), all far below either
//! network layer's MTU, so an SDU that does not fit is a modelling error or a deliberately
//! oversize attack payload, and splitting it silently would hide both.
//!
//! An oversize SDU is refused with [`DropCause::Mtu`], which 04-models.md §7.3 names
//! explicitly. Nothing is buffered, nothing times out, and the loss amplification is the
//! degenerate case `n = 1`, so `P_sdu = p`.

use v2xw_core::card::{
    Determinism, Family, ModelCard, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::ctx::{Ctx, CtxExt};
use v2xw_core::ids::{NodeId, SduId};
use v2xw_core::model::Model;
use v2xw_core::time::Duration;

use crate::amplification;
use crate::error::DropCause;
use crate::frag::{FragRecord, FragmentDesc, Fragmenter, ReassemblyOutcome};

/// This model's stable id.
pub const FRAGMENTER_NONE_ID: &str = "fragmenter/none";

/// `fragmenter/none`: an SDU above the MTU is dropped, with a cause.
///
/// [`Fragmenter::fragment`] needs a context type to name the trait, so the split is also
/// reachable as the inherent [`NoneFragmenter::split`], which is what the trait method
/// calls:
///
/// ```
/// use v2xw_core::ids::SduId;
/// use v2xw_net::error::DropCause;
/// use v2xw_net::frag::none::NoneFragmenter;
///
/// let f = NoneFragmenter::default();
/// // A 357 B secured CAM under GeoNetworking's 1,398 B MTU: one whole SDU.
/// let one = f.split(SduId::new(1), 357, 1_398).unwrap();
/// assert_eq!(one.len(), 1);
/// assert!(one[0].is_whole());
///
/// // A 2,000 B SDU: refused, with the cause 04-models.md §7.3 names.
/// assert_eq!(
///     f.split(SduId::new(2), 2_000, 1_398),
///     Err(DropCause::Mtu { sdu_bytes: 2_000, mtu: 1_398 }),
/// );
/// ```
#[derive(Debug, Clone)]
pub struct NoneFragmenter {
    card: ModelCard,
}

impl Default for NoneFragmenter {
    fn default() -> Self {
        Self::new()
    }
}

impl NoneFragmenter {
    /// The model. It has no parameters.
    pub fn new() -> Self {
        Self { card: card() }
    }

    /// Splits an SDU — the body of [`Fragmenter::fragment`], callable without naming a
    /// context type.
    ///
    /// # Errors
    /// [`DropCause::Mtu`] when the SDU exceeds the MTU, which is this model's whole
    /// behaviour (04-models.md §7.3).
    pub fn split(
        &self,
        sdu: SduId,
        sdu_bytes: u32,
        mtu: u32,
    ) -> core::result::Result<Vec<FragmentDesc>, DropCause> {
        if sdu_bytes > mtu {
            return Err(DropCause::Mtu { sdu_bytes, mtu });
        }
        Ok(vec![FragmentDesc::whole(sdu, sdu_bytes)])
    }
}

impl Model for NoneFragmenter {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C> Fragmenter<C> for NoneFragmenter
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
        let outcome = if frag.is_whole() {
            ReassemblyOutcome::Complete {
                sdu: frag.sdu,
                bytes: frag.payload_bytes,
                segments: 1,
            }
        } else {
            // The sender fragmented and this receiver's strategy does not reassemble: a
            // configuration mismatch, reported rather than papered over.
            ReassemblyOutcome::Failed {
                sdu: frag.sdu,
                cause: DropCause::UnexpectedFragment {
                    index: frag.index,
                    count: frag.count,
                },
            }
        };
        if crate::frag::should_record(frag, &outcome) {
            ctx.emit(FragRecord::new(ctx.now(), rx, &outcome));
        }
        outcome
    }

    fn overhead_bytes(&self) -> u32 {
        0
    }

    fn reassembly_timeout(&self) -> Option<Duration> {
        None
    }
}

/// The model card for `fragmenter/none`.
fn card() -> ModelCard {
    let mut card = ModelCard::new(
        FRAGMENTER_NONE_ID,
        Family::Fragmenter,
        "1.0.0",
        "Does not fragment: an SDU above the MTU is dropped with DropCause::Mtu. The \
         default for BSM and CAM, whose sizes are far below either network layer's MTU.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![amplification::equation()];
    card.assumptions = vec![
        "The message set this strategy carries fits the MTU: a BSM SPDU is 180-250 B and a \
         secured CAM 182-807 B (04-models.md §8.2, §9.3), against 1,398 B for \
         GeoNetworking and 1,400 B for the WSMP MIB default. An SDU above the MTU is \
         therefore a modelling error or a deliberately oversize payload, and both are \
         better reported than split."
            .to_string(),
        "No reassembly state exists, so nothing can time out. Invariant I-N2's reassembly \
         timeout is 'none' for this model, which Fragmenter::reassembly_timeout returns as \
         None."
            .to_string(),
    ];
    card.limitations = {
        let mut l = vec![
            "There is no loss amplification to report: n = 1, so P_sdu = p. The formula is \
             on this card so that a reader comparing the four strategies sees the same one \
             (invariant I-N2)."
                .to_string(),
            "A fragment from a peer configured with a different strategy is refused with \
             DropCause::UnexpectedFragment rather than reassembled."
                .to_string(),
        ];
        l.extend(amplification::ASSUMPTIONS.iter().map(|s| (*s).to_string()));
        l
    };
    card.ignores = vec![
        "Every form of fragmentation: facilities-layer segmentation, certificate-cycle \
         fragmentation and generic SDU fragmentation are the other three models of \
         04-models.md §7.3."
            .to_string(),
    ];
    card.sources = vec![Source::new(
        SourceKind::Paper,
        "04-models.md §7.3 — 'fragmenter/none: oversize SDUs are rejected \
         (DropCause::Mtu); none; the default for BSM and CAM'",
    )];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "frag::none::tests::an_oversize_sdu_is_rejected_with_the_mtu_cause".to_string(),
            "frag::none::tests::an_sdu_within_the_mtu_passes_through_whole".to_string(),
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
    use crate::frag::FragmentKind;
    use crate::testctx::TestCtx;

    fn model() -> NoneFragmenter {
        NoneFragmenter::new()
    }

    fn frag(
        f: &NoneFragmenter,
        bytes: u32,
        mtu: u32,
    ) -> core::result::Result<Vec<FragmentDesc>, DropCause> {
        <NoneFragmenter as Fragmenter<TestCtx>>::fragment(f, SduId::new(1), bytes, mtu)
    }

    /// The test 04-models.md §7.3 asks for: an oversize SDU is rejected with the right
    /// cause.
    #[test]
    fn an_oversize_sdu_is_rejected_with_the_mtu_cause() {
        let f = model();
        // One byte over GeoNetworking's itsGnMaxSduSize.
        assert_eq!(
            frag(&f, 1_399, 1_398),
            Err(DropCause::Mtu {
                sdu_bytes: 1_399,
                mtu: 1_398
            })
        );
        // …and one byte over the WSMP MIB default.
        assert_eq!(
            frag(&f, 1_401, 1_400),
            Err(DropCause::Mtu {
                sdu_bytes: 1_401,
                mtu: 1_400
            })
        );
        // The message names both numbers, which is what makes the drop diagnosable.
        let text = frag(&f, 2_000, 1_398).unwrap_err().to_string();
        assert!(text.contains("2000") && text.contains("1398"), "{text}");
        assert!(text.contains("does not fragment"), "{text}");
    }

    #[test]
    fn an_sdu_within_the_mtu_passes_through_whole() {
        let f = model();
        for bytes in [0u32, 1, 180, 250, 357, 807, 1_398] {
            let parts = frag(&f, bytes, 1_398).expect("within the MTU");
            assert_eq!(parts.len(), 1);
            assert_eq!(parts[0].kind, FragmentKind::Whole);
            assert_eq!(parts[0].payload_bytes, bytes);
            assert_eq!(parts[0].header_bytes, 0, "no fragmentation overhead");
            assert_eq!(parts[0].total_bytes(), bytes);
            assert!(parts[0].is_whole());
        }
        assert_eq!(
            <NoneFragmenter as Fragmenter<TestCtx>>::overhead_bytes(&f),
            0
        );
        assert_eq!(
            <NoneFragmenter as Fragmenter<TestCtx>>::reassembly_timeout(&f),
            None
        );
    }

    #[test]
    fn a_whole_sdu_is_delivered_and_a_fragment_is_refused() {
        let mut f = model();
        let mut ctx = TestCtx::new();
        let peer = NodeId::new(4);
        let rx = NodeId::new(9);

        let whole = FragmentDesc::whole(SduId::new(3), 357);
        assert_eq!(
            f.reassemble(&mut ctx, rx, &whole, peer),
            ReassemblyOutcome::Complete {
                sdu: SduId::new(3),
                bytes: 357,
                segments: 1
            }
        );

        let piece = FragmentDesc {
            index: 1,
            count: 3,
            kind: FragmentKind::Piece,
            ..whole
        };
        assert_eq!(
            f.reassemble(&mut ctx, rx, &piece, peer),
            ReassemblyOutcome::Failed {
                sdu: SduId::new(3),
                cause: DropCause::UnexpectedFragment { index: 1, count: 3 }
            }
        );

        // Only the failure is recorded: an unfragmented SDU passing through is not a
        // fragmentation event, and recording one per received message would flood the
        // channel (see frag::should_record).
        let records = ctx.records_on("net.frag");
        assert_eq!(records.len(), 1);
        assert!(records[0].contains("\"outcome\":\"failed\""));
    }

    #[test]
    fn a_single_fragment_does_not_amplify_loss() {
        let f = model();
        let m = <NoneFragmenter as Fragmenter<TestCtx>>::loss_amplification(
            &f,
            &[FragmentLoss::new(0, 0.1, 357)],
        );
        assert_eq!(m.fragments, 1);
        assert!(
            (m.p_sdu_lost() - 0.1).abs() < 1e-15,
            "n = 1 means P_sdu = p"
        );
        assert!((m.amplification_factor(0.1) - 1.0).abs() < 1e-15);
    }

    #[test]
    fn the_card_validates_and_states_the_amplification_terms() {
        let f = model();
        let card = f.card();
        card.validate().expect("card validates");
        card.check_api_version().expect("api version matches");
        assert_eq!(card.family, Family::Fragmenter);
        assert!(!card.determinism.uses_rng);
        assert!(
            card.equations
                .iter()
                .any(|e| e.latex_or_text.contains("1 - prod_i (1 - p_i)")),
            "invariant I-N2: the card states the amplification formula"
        );
        for a in amplification::ASSUMPTIONS {
            assert!(card.limitations.iter().any(|l| l == a));
        }
    }
}
