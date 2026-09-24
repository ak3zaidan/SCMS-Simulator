//! Fragmentation in the run path (04-models.md §7.3, §7.4).
//!
//! `v2xw-net` implements the four strategies of §7.3 as models; this module is how the
//! engine *runs* them. Neither WSMP nor GeoNetworking can split an SDU, so every split
//! happens above the network layer, at the sender's hand-down, and every reassembly at the
//! receiver, between the PHY's decision and the node's receive queue:
//!
//! | Strategy | At the sender | At the receiver |
//! |---|---|---|
//! | `fragmenter/none` | a signed message above the MTU is refused and counted | — |
//! | `fragmenter/generic-sdu` | an SDU above the MTU goes out as `n` frames, each with a 4-octet header | the fragments are reassembled per (sender, SDU); the message reaches the node's queue when the last one arrives, or is lost at the reassembly timeout |
//! | `fragmenter/facilities-segmentation` | an SDU above the segmentation threshold goes out as `n` independently interpretable segments, each repeating its containers | each segment is a message of its own and reaches the node's queue as it arrives |
//! | `fragmenter/cert-cycle-partial-hybrid` | the hybrid certificate rides in the first α SPDUs of each τ-SPDU cycle, adding its fragment's octets to those frames | the certificate is reassembled per (sender, cycle); the messages themselves are ordinary |
//!
//! Every fragmented SDU is followed, per receiver, to one **reassembly group** ([`FragGroup`])
//! and resolved exactly once — complete, or lost at the strategy's timeout — onto the
//! `net.reassembly` channel with the loss its fragments' own PHY success probabilities
//! predicted, `1 − Π (1 − p_i)` (§7.4, [`v2xw_net::amplification::sdu_loss`]). The gap between
//! the realised and the predicted loss is the visible signature of correlated fragment
//! loss, which is why the prediction is recorded next to the outcome rather than computed
//! after the fact from averages.
//!
//! # The oversize payload knob
//!
//! No signature this build can select makes a message larger than the MTU (a CAM with a
//! certificate is about 400 octets), and post-quantum signatures are not selectable yet. So
//! `net.fragmenter.params.sdu_padding_bytes` appends that many opaque octets to every
//! signed message before the fragmenter sees it — standing in for a hybrid signature or
//! certificate — and counts them as payload. It is an engine parameter, not the
//! fragmenter's: it is stripped before the model's own parameters are resolved against
//! its card.

use v2xw_core::ids::{NodeId, SduId};
use v2xw_core::registry::ParamSet;
use v2xw_core::time::{Duration, SimTime};
use v2xw_net::amplification::{FragmentLoss, sdu_loss};
use v2xw_net::frag::{FragmentDesc, Fragmenter, ReassemblyOutcome};
use v2xw_net::{
    CertCyclePartialHybrid, DropCause, FRAGMENTER_CERT_CYCLE_ID, FRAGMENTER_FACILITIES_ID,
    FRAGMENTER_GENERIC_ID, FRAGMENTER_NONE_ID, FacilitiesSegmentation, GenericSduFragmenter,
};

use crate::records::NodeRx;
use crate::scenario::ModelChoice;

/// The engine parameter inside `net.fragmenter.params` that pads every signed message.
pub const SDU_PADDING_PARAM: &str = "sdu_padding_bytes";

/// The largest padding a scenario may ask for, octets: one 255-fragment generic SDU at the
/// WSMP MTU is about 355 kB, and a padding far beyond any post-quantum certificate
/// (SPHINCS+ signatures are under 50 kB) is a typo rather than a study.
pub const SDU_PADDING_MAX: u64 = 65_535;

/// A splitting strategy, as the engine holds it: one prototype for the senders' splits,
/// cloned per receiver for the reassembly state.
#[derive(Debug, Clone)]
pub enum Strategy {
    /// `fragmenter/generic-sdu`.
    Generic(GenericSduFragmenter),
    /// `fragmenter/facilities-segmentation`.
    Facilities(FacilitiesSegmentation),
    /// `fragmenter/cert-cycle-partial-hybrid`.
    CertCycle(CertCyclePartialHybrid),
}

/// What a reassembly group reassembles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GroupKind {
    /// An SDU in pieces that mean nothing alone: the message reaches the node only whole.
    Message,
    /// An SDU in independently interpretable segments: each is a message of its own.
    Segments,
    /// A hybrid certificate spread over a certificate cycle's SPDUs.
    Certificate,
}

impl GroupKind {
    /// The label on `net.reassembly`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            GroupKind::Message => "message",
            GroupKind::Segments => "segments",
            GroupKind::Certificate => "certificate",
        }
    }
}

/// The scenario's fragmentation choice, resolved.
#[derive(Debug, Clone)]
pub struct FragPlan {
    /// The model id.
    pub id: String,
    /// The splitting strategy, or `None` for `fragmenter/none`.
    pub strategy: Option<Strategy>,
    /// Octets appended to every signed message (see the module header).
    pub padding: u32,
}

impl FragPlan {
    /// `fragmenter/none` with no padding: what a scenario without the key runs.
    #[must_use]
    pub fn none() -> Self {
        FragPlan {
            id: FRAGMENTER_NONE_ID.to_string(),
            strategy: None,
            padding: 0,
        }
    }

    /// The plan a `net.fragmenter` choice names.
    ///
    /// # Errors
    /// A message naming the offending parameter or id: an unknown model, a padding that is
    /// not an integer in `[0, SDU_PADDING_MAX]`, or a model parameter its card refuses.
    pub fn from_choice(choice: &ModelChoice) -> Result<Self, String> {
        let mut overrides = match &choice.params {
            serde_json::Value::Null => serde_json::Map::new(),
            serde_json::Value::Object(m) => m.clone(),
            _ => return Err("params must be an object".to_string()),
        };
        let padding = match overrides.remove(SDU_PADDING_PARAM) {
            None => 0,
            Some(v) => match v.as_u64() {
                Some(n) if n <= SDU_PADDING_MAX => n as u32,
                _ => {
                    return Err(format!(
                        "params.{SDU_PADDING_PARAM} is {v}, and must be an integer number of \
                         octets in [0, {SDU_PADDING_MAX}]"
                    ));
                }
            },
        };
        let overrides = serde_json::Value::Object(overrides);
        let resolve = |card: &v2xw_core::card::ModelCard| {
            ParamSet::resolve(card, &overrides).map_err(|e| format!("params: {e}"))
        };
        let strategy = match choice.id.as_str() {
            FRAGMENTER_NONE_ID => {
                if overrides.as_object().is_some_and(|m| !m.is_empty()) {
                    return Err("params: fragmenter/none has no parameters of its own".to_string());
                }
                None
            }
            FRAGMENTER_GENERIC_ID => {
                let card = v2xw_core::model::Model::card(&GenericSduFragmenter::default()).clone();
                Some(Strategy::Generic(GenericSduFragmenter::from_params(
                    &resolve(&card)?,
                )))
            }
            FRAGMENTER_FACILITIES_ID => {
                let card =
                    v2xw_core::model::Model::card(&FacilitiesSegmentation::default()).clone();
                Some(Strategy::Facilities(FacilitiesSegmentation::from_params(
                    &resolve(&card)?,
                )))
            }
            FRAGMENTER_CERT_CYCLE_ID => {
                let card =
                    v2xw_core::model::Model::card(&CertCyclePartialHybrid::default()).clone();
                Some(Strategy::CertCycle(CertCyclePartialHybrid::from_params(
                    &resolve(&card)?,
                )))
            }
            other => {
                return Err(format!(
                    "id '{other}' is not a fragmenter; the fragmenters are '{FRAGMENTER_NONE_ID}', \
                     '{FRAGMENTER_GENERIC_ID}', '{FRAGMENTER_FACILITIES_ID}' and \
                     '{FRAGMENTER_CERT_CYCLE_ID}'"
                ));
            }
        };
        Ok(FragPlan {
            id: choice.id.clone(),
            strategy,
            padding,
        })
    }

    /// How an SDU of `bytes` octets goes on the air over a network layer of MTU `mtu`: as
    /// one whole frame or as `n` pieces or segments. `None` for the strategies that never
    /// split a message (`none`, and the certificate cycle, which splits the certificate).
    ///
    /// # Errors
    /// The strategy's own refusal ([`DropCause`]).
    pub fn split(
        &self,
        sdu: SduId,
        bytes: u32,
        mtu: u32,
    ) -> Option<Result<Vec<FragmentDesc>, DropCause>> {
        match self.strategy.as_ref()? {
            Strategy::Generic(g) => Some(g.split(sdu, bytes, mtu)),
            Strategy::Facilities(f) => Some(f.split(sdu, bytes, mtu)),
            Strategy::CertCycle(_) => None,
        }
    }

    /// The certificate cycle, when that is the strategy.
    #[must_use]
    pub fn cert_cycle(&self) -> Option<&CertCyclePartialHybrid> {
        match self.strategy.as_ref()? {
            Strategy::CertCycle(c) => Some(c),
            _ => None,
        }
    }

    /// What a group of this strategy reassembles.
    #[must_use]
    pub fn kind(&self) -> Option<GroupKind> {
        Some(match self.strategy.as_ref()? {
            Strategy::Generic(_) => GroupKind::Message,
            Strategy::Facilities(_) => GroupKind::Segments,
            Strategy::CertCycle(_) => GroupKind::Certificate,
        })
    }

    /// How long a receiver waits for a group's missing fragments: the reassembly timeout
    /// (generic), the certificate cycle's period, or — for segments, which have no
    /// reassembly state — the GeoNetworking maximum packet lifetime within which an
    /// event's segments are on the air (TS 103 324 §5.3.3).
    #[must_use]
    pub fn group_timeout(&self) -> Duration {
        match self.strategy.as_ref() {
            Some(Strategy::Generic(g)) => Duration::from_millis(g.params().reassembly_timeout_ms),
            Some(Strategy::Facilities(f)) => f.max_packet_lifetime(),
            Some(Strategy::CertCycle(c)) => Duration::from_millis(c.params().cycle_period_ms),
            None => Duration::ZERO,
        }
    }

    /// A fresh reassembler for one receiver.
    #[must_use]
    pub fn reassembler(&self) -> Option<Strategy> {
        self.strategy.clone()
    }
}

impl Strategy {
    /// [`Fragmenter::reassemble`] on whichever model this is.
    pub fn reassemble<C: v2xw_core::ctx::Ctx + ?Sized>(
        &mut self,
        ctx: &mut C,
        rx: NodeId,
        frag: &FragmentDesc,
        from: NodeId,
    ) -> ReassemblyOutcome {
        match self {
            Strategy::Generic(g) => g.reassemble(ctx, rx, frag, from),
            Strategy::Facilities(f) => f.reassemble(ctx, rx, frag, from),
            Strategy::CertCycle(c) => c.reassemble(ctx, rx, frag, from),
        }
    }

    /// [`Fragmenter::expire`] on whichever model this is.
    pub fn expire<C: v2xw_core::ctx::Ctx + ?Sized>(
        &mut self,
        ctx: &mut C,
        rx: NodeId,
        now: SimTime,
    ) -> Vec<ReassemblyOutcome> {
        match self {
            Strategy::Generic(g) => g.expire(ctx, rx, now),
            Strategy::Facilities(f) => f.expire(ctx, rx, now),
            Strategy::CertCycle(c) => c.expire(ctx, rx, now),
        }
    }

    /// Whether losing one fragment loses the whole group.
    #[must_use]
    pub fn amplifies_loss(&self) -> bool {
        !matches!(self, Strategy::Facilities(_))
    }
}

/// What a frame carrying a fragment knows about the SDU it belongs to.
#[derive(Debug, Clone, Copy)]
pub struct FragMeta {
    /// The fragment.
    pub desc: FragmentDesc,
    /// What its group reassembles.
    pub kind: GroupKind,
    /// The message id the group is recorded under: the frame index of the SDU's first
    /// fragment (or of the certificate cycle's first SPDU).
    pub sdu_msg: u64,
    /// The SDU's octets as the receiver verifies them: the signed message, padded.
    pub sdu_bytes: u32,
    /// The SDU's payload octets, for the content-loss figure.
    pub sdu_payload: u32,
    /// Every fragment's PSDU octets together: what the SDU cost on the air.
    pub psdu_total: u32,
    /// Every fragment's air time together, µs.
    pub air_total_us: u64,
}

/// One fragmented SDU at one receiver, from its first fragment attempt to its resolution.
#[derive(Debug, Clone)]
pub struct FragGroup {
    /// What it reassembles.
    pub kind: GroupKind,
    /// The sender (ground truth).
    pub tx: NodeId,
    /// The message id it is recorded under.
    pub sdu_msg: u64,
    /// The message type, for the per-type breakdown.
    pub msg_type: &'static str,
    /// How many fragments the SDU was split into.
    pub fragments: u16,
    /// The SDU's octets: what its fragments carry between them (the signed message and
    /// its padding, or the hybrid certificate).
    pub payload_total: u32,
    /// Per fragment index that reached this receiver's arrival set: its PHY success
    /// probability and whether it decoded.
    pub seen: std::collections::BTreeMap<u16, (f64, bool, u32)>,
    /// The first PHY loss cause among its fragments, in arrival order.
    pub first_cause: Option<&'static str>,
    /// When the receiver gives up on the missing fragments.
    pub deadline: SimTime,
    /// Whether the reassembler holds state for it (a fragment decoded).
    pub opened: bool,
    /// For [`GroupKind::Message`]: the `node.rx` attempt the group resolves, built from its
    /// first fragment attempt and carrying the SDU's size.
    pub attempt: Option<NodeRx>,
}

impl FragGroup {
    /// How many fragments decoded here.
    #[must_use]
    pub fn received(&self) -> u16 {
        self.seen.values().filter(|(_, ok, _)| *ok).count() as u16
    }

    /// The SDU payload octets the decoded fragments carried.
    #[must_use]
    pub fn payload_received(&self) -> u32 {
        self.seen
            .values()
            .filter(|(_, ok, _)| *ok)
            .map(|(_, _, b)| *b)
            .sum()
    }

    /// Whether every fragment decoded here.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.received() == self.fragments
    }

    /// What 04-models.md §7.4 predicts for this group from its fragments' PHY success
    /// probabilities, assuming they are lost independently: `1 − Π (1 − p_i)` that some
    /// fragment is lost, and the payload-weighted expected content loss. A fragment that
    /// never reached this receiver's arrival set (out of range, or dropped at the sender)
    /// is certainly lost, `p_i = 1`.
    #[must_use]
    pub fn prediction(&self, amplifies: bool) -> v2xw_net::amplification::SduLossModel {
        let per_fragment: Vec<FragmentLoss> = (0..self.fragments)
            .map(|index| {
                let (psr, bytes) = match self.seen.get(&index) {
                    Some((psr, _, bytes)) => (*psr, *bytes),
                    // Unseen: the size is unknown here, so the equal share stands in; it
                    // only weights the content-loss mean, never the SDU loss.
                    None => (0.0, self.payload_total / u32::from(self.fragments.max(1))),
                };
                FragmentLoss {
                    index,
                    p: 1.0 - psr,
                    payload_bytes: bytes,
                }
            })
            .collect();
        sdu_loss(&per_fragment, amplifies)
    }
}
