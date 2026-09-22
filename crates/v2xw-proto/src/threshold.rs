//! The hook for an interactive threshold protocol — the shape, not a scheme.
//!
//! The owner intends a post-quantum threshold/umbrella protocol and has not supplied its
//! specification (05-protocols.md §5 records this as `[FILL IN]` in the brief §8.3). This
//! module therefore defines the *surface* such a protocol needs and proves the shape with
//! one trivial implementation. **It invents no scheme and no number.** The one
//! implementation here carries FROST's round structure and byte counts from RFC 9591 §5,
//! cited, and says in its own documentation that it performs no cryptography: it exists so
//! that the engine seam, the round accounting and the stage timestamps can be exercised
//! before the real specification arrives.
//!
//! # What the real one will need
//!
//! Everything below is already expressible; these are the things whose *values* only the
//! specification can supply.
//!
//! * **Round counts per operation.** [`RoundPlan`] carries them per round, so a scheme
//!   with 3 rounds of DKG and 19–149 online signing rounds (the range 05-protocols §5.2
//!   records across CGGMP21, FROST, Threshold Raccoon, CELI25 and Quorus) needs no
//!   interface change.
//! * **Per-round message sizes, which are pairwise for some schemes and broadcast for
//!   others.** [`RoundMessage`] names both endpoints, and a broadcast is `n − 1` messages,
//!   because that is what it costs on a real network.
//! * **Abort and retry probability.** CELI25's threshold ML-DSA expects 4–5 attempts per
//!   signature; [`RoundPlan::attempts`] is where that number goes, and it must come from
//!   the scheme, not from here.
//! * **Whether signing is presignable.** [`ThresholdProtocol::presign_rounds`] exists for
//!   the CGGMP21-style split of 3 offline rounds plus 1 online round.
//! * **Share refresh cadence and its effect on revocation.** [`RefreshPolicy`] holds the
//!   epoch length; what a refresh does to a revoked share is a scheme question.
//! * **The umbrella relation.** [`UmbrellaScheme`] says how many leaf pseudonyms one
//!   umbrella credential covers and how the envelope carries it; the real scheme decides
//!   whether a leaf is derived locally (no round trip) or issued (one).
//!
//! What the engine does *not* need to learn: a round completes only when its last message
//! has been delivered and charged, which is [`crate::kernel`]'s behaviour already. There
//! is no shortcut for "the committee signs".

use v2xw_core::ids::NodeId;
use v2xw_core::time::Duration;

use crate::sizes::WireSize;
use crate::spec::{Centrality, OpDescriptor};
use crate::stage::FlowId;

/// One message inside one round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundMessage {
    /// The sender's index within the committee.
    pub from: u16,
    /// The receiver's index, or `None` for a broadcast that the planner will expand.
    pub to: Option<u16>,
    /// Its size.
    pub bytes: WireSize,
}

/// One round: who sends what to whom, and what each participant computes.
#[derive(Debug, Clone)]
pub struct RoundPlan {
    /// Which round this is, from 1.
    pub index: u16,
    /// The messages it carries.
    pub messages: Vec<RoundMessage>,
    /// What each participant computes in it.
    pub compute: Vec<OpDescriptor>,
    /// How many attempts the scheme expects before this round succeeds. `1` for a scheme
    /// that never aborts; CELI25's threshold ML-DSA needs 4–5 [CELI25; QUORUS §5].
    pub attempts: u16,
}

impl RoundPlan {
    /// The bytes this round puts on the network, expanding broadcasts over `n`
    /// participants.
    pub fn bytes(&self, n: u16) -> u64 {
        self.messages
            .iter()
            .map(|m| {
                let copies = u64::from(if m.to.is_some() {
                    1
                } else {
                    n.saturating_sub(1)
                });
                copies * u64::from(m.bytes.bytes())
            })
            .sum::<u64>()
            .saturating_mul(u64::from(self.attempts))
    }
}

/// How often shares are refreshed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshPolicy {
    /// The epoch length.
    pub epoch: Duration,
    /// Whether a refresh invalidates the previous epoch's shares outright.
    pub invalidates_previous: bool,
}

/// The committee.
#[derive(Debug, Clone)]
pub struct Committee {
    /// Committee size.
    pub n: u16,
    /// Threshold: `t + 1` participants must act.
    pub t: u16,
    /// The nodes, in committee-index order.
    pub members: Vec<NodeId>,
    /// The coordinator, for schemes that need one (FROST does [RFC9591 §5]).
    pub coordinator: Option<NodeId>,
}

impl Committee {
    /// How the committee appears to the role specification.
    pub const fn centrality(&self) -> Centrality {
        Centrality::Distributed {
            n: self.n,
            t: self.t,
        }
    }
}

/// The surface an interactive multi-round protocol needs.
///
/// Three operations, each a list of rounds. Nothing here says what the rounds *contain* —
/// that is the scheme's business — and nothing here lets a participant skip one.
pub trait ThresholdProtocol {
    /// The scheme's stable name.
    fn name(&self) -> &'static str;

    /// Distributed key generation.
    fn dkg_rounds(&self, committee: &Committee) -> Vec<RoundPlan>;

    /// Offline presignature rounds, for a scheme that has them.
    fn presign_rounds(&self, _committee: &Committee) -> Vec<RoundPlan> {
        Vec::new()
    }

    /// Online signing rounds for one signature.
    fn sign_rounds(&self, committee: &Committee) -> Vec<RoundPlan>;

    /// Proactive share refresh.
    fn refresh_rounds(&self, committee: &Committee) -> Vec<RoundPlan>;

    /// The refresh cadence.
    fn refresh_policy(&self) -> RefreshPolicy;

    /// The signature's size on the wire.
    fn signature_bytes(&self) -> WireSize;

    /// The verification key's size.
    fn verification_key_bytes(&self) -> WireSize;

    /// Which flow each operation is recorded under.
    fn flow_of(&self, op: ThresholdOp) -> FlowId {
        match op {
            ThresholdOp::Dkg => FlowId::ThresholdDkg,
            ThresholdOp::Sign | ThresholdOp::Presign => FlowId::ThresholdSign,
            ThresholdOp::Refresh => FlowId::ThresholdRefresh,
        }
    }

    /// Total bytes one operation costs the network.
    fn bytes_of(&self, op: ThresholdOp, committee: &Committee) -> u64 {
        let rounds = match op {
            ThresholdOp::Dkg => self.dkg_rounds(committee),
            ThresholdOp::Presign => self.presign_rounds(committee),
            ThresholdOp::Sign => self.sign_rounds(committee),
            ThresholdOp::Refresh => self.refresh_rounds(committee),
        };
        rounds.iter().map(|r| r.bytes(committee.n)).sum()
    }

    /// How many rounds one operation takes, counting expected attempts.
    fn rounds_of(&self, op: ThresholdOp, committee: &Committee) -> u32 {
        let rounds = match op {
            ThresholdOp::Dkg => self.dkg_rounds(committee),
            ThresholdOp::Presign => self.presign_rounds(committee),
            ThresholdOp::Sign => self.sign_rounds(committee),
            ThresholdOp::Refresh => self.refresh_rounds(committee),
        };
        rounds.iter().map(|r| u32::from(r.attempts)).sum()
    }
}

/// Which operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThresholdOp {
    /// Distributed key generation.
    Dkg,
    /// Offline presignature generation.
    Presign,
    /// Online signing.
    Sign,
    /// Share refresh.
    Refresh,
}

/// How an umbrella credential relates to the pseudonyms it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UmbrellaScheme {
    /// How many leaf pseudonyms one umbrella covers.
    pub leaves_per_umbrella: u32,
    /// Whether a leaf is derived on the device (no round trip) or issued (at least one).
    pub leaf_is_derived_locally: bool,
    /// The umbrella credential's size.
    pub umbrella_bytes: WireSize,
    /// A leaf's size.
    pub leaf_bytes: WireSize,
}

/// A shape proof, and **not** a cryptographic implementation.
///
/// The round structure and the byte counts are FROST's, from RFC 9591 §5: two rounds (one
/// with preprocessing), 64–66 B of commitments in round one and a 32 B signature share in
/// round two, with a coordinator. Key generation and refresh are given the round counts
/// their cited sources give — three for a CGGMP21-style DKG and refresh [CGGMP21 Table 1],
/// one private message plus one broadcast for a Herzberg-style proactive refresh [HJKY95]
/// — and nothing else. **No key is generated, no share is computed and nothing is signed
/// here.** Its purpose is to let the engine seam, the round accounting and the stage
/// timestamps be exercised before the owner's specification arrives; a run that used it
/// would be measuring a network and a queue, not a cryptosystem, and the model card says
/// so.
#[derive(Debug, Clone)]
pub struct FrostShapedPlaceholder {
    /// The refresh cadence, which no source fixes: a scenario parameter.
    pub refresh: RefreshPolicy,
}

impl Default for FrostShapedPlaceholder {
    fn default() -> FrostShapedPlaceholder {
        FrostShapedPlaceholder {
            refresh: RefreshPolicy {
                epoch: Duration::from_secs(86_400),
                invalidates_previous: true,
            },
        }
    }
}

const RFC9591: &str = "RFC 9591 §5 (FROST): round 1 commitments 64-66 B, round 2 share 32 B";
const CGGMP21: &str = "CGGMP21 (ePrint 2021/060) Table 1: 3 rounds of key generation";
const HJKY95: &str = "Herzberg et al., CRYPTO 1995: proactive refresh is one private message to each other \
     party plus one broadcast";

impl ThresholdProtocol for FrostShapedPlaceholder {
    fn name(&self) -> &'static str {
        "threshold/frost-shaped-placeholder"
    }

    fn dkg_rounds(&self, committee: &Committee) -> Vec<RoundPlan> {
        (1..=3)
            .map(|index| RoundPlan {
                index,
                messages: vec![RoundMessage {
                    from: 0,
                    to: None,
                    bytes: WireSize::cited(
                        4 * 32,
                        "CGGMP21 Table 1: n · 4κ incoming per party at κ = 256 bit",
                    ),
                }],
                compute: Vec::new(),
                attempts: 1,
            })
            .take(if committee.n > 1 { 3 } else { 0 })
            .collect()
    }

    fn sign_rounds(&self, committee: &Committee) -> Vec<RoundPlan> {
        let signers = committee.t.saturating_add(1);
        vec![
            RoundPlan {
                index: 1,
                messages: (0..signers)
                    .map(|from| RoundMessage {
                        from,
                        to: Some(u16::MAX),
                        bytes: WireSize::cited(66, RFC9591),
                    })
                    .collect(),
                compute: Vec::new(),
                attempts: 1,
            },
            RoundPlan {
                index: 2,
                messages: (0..signers)
                    .map(|from| RoundMessage {
                        from,
                        to: Some(u16::MAX),
                        bytes: WireSize::cited(32, RFC9591),
                    })
                    .collect(),
                compute: Vec::new(),
                attempts: 1,
            },
        ]
    }

    fn refresh_rounds(&self, committee: &Committee) -> Vec<RoundPlan> {
        vec![
            RoundPlan {
                index: 1,
                messages: (0..committee.n)
                    .map(|from| RoundMessage {
                        from,
                        to: None,
                        bytes: WireSize::cited(32, HJKY95),
                    })
                    .collect(),
                compute: Vec::new(),
                attempts: 1,
            },
            RoundPlan {
                index: 2,
                messages: (0..committee.n)
                    .map(|from| RoundMessage {
                        from,
                        to: None,
                        bytes: WireSize::cited(33, CGGMP21),
                    })
                    .collect(),
                compute: Vec::new(),
                attempts: 1,
            },
        ]
    }

    fn refresh_policy(&self) -> RefreshPolicy {
        self.refresh
    }

    fn signature_bytes(&self) -> WireSize {
        WireSize::cited(
            64,
            "RFC 9591: a FROST signature is a Schnorr signature over the group, 64 B for P-256",
        )
    }

    fn verification_key_bytes(&self) -> WireSize {
        WireSize::cited(33, "SEC 1 §2.3.3: compressed P-256 point")
    }
}
