//! [`NodeView`] — the belief-only handle, and the firewall that enforces invariant I-C2.
//!
//! > **I-C2** A plug-in may not read `World` ground truth for a node's *belief* unless the
//! > interface explicitly passes it (e.g., `Mobility` gets GT; `Detector` does not). The
//! > Python SDK enforces this by handing `Detector` a `NodeView`, not the world.
//! > — 03-interfaces.md §1
//!
//! **This type is that enforcement.** A misbehaviour detector, a message generator, a
//! safety application and an attacker all run *as a node*: everything they may use is
//! something that node could itself have observed — its own GNSS belief, its own clock, the
//! messages it received and verified, the neighbour table it built from them, the
//! credentials it holds. `NodeView` is the whole of that, and it has no `world()`, no
//! `actors()`, no `kinematics()` and no [`crate::ids::ActorId`] anywhere in its surface.
//!
//! # The bug this prevents
//!
//! Hand a detector a `&World` (or a [`crate::ctx::Ctx`] whose `world()` it can reach) and
//! it will work — beautifully. It will resolve the sender of a suspicious CAM to the actor
//! that really sent it, compare the claimed position against the true one, and report
//! every attacker with no false positives. The run will be deterministic, the tests will
//! pass, the metrics will look extraordinary, and the result will be worthless: no real
//! receiver can do that, so the detection rate measured is not a detection rate. The same
//! leak through an exported feature column silently teaches a machine-learning model to
//! read ground truth (08-measurement-and-data.md's leakage rule), and the model then
//! scores near-perfectly on the dataset and near-randomly in the world.
//!
//! The failure is quiet. Nothing crashes, no number looks wrong, and the only symptom is a
//! result that is too good — which is not a symptom anybody reports. So the defence is
//! structural rather than procedural: the *argument type* of the call must not be able to
//! reach the truth. If a family trait's method takes `&dyn NodeView<…>`, an implementation
//! that wants ground truth cannot get it, and one that tries fails to compile.
//!
//! The same reasoning is why invariant I-T1 says an `Attacker` "receives no `World` or
//! `ActorIndex` reference" and why I-T2 says detectors and MA pipelines "run with the
//! node's belief only". An attacker that knew the truth would falsify optimally, which
//! overstates the threat exactly as a truth-reading detector overstates the defence.
//!
//! # What it deliberately does *not* have
//!
//! | Not here | Where it is | Why not here |
//! |---|---|---|
//! | `world()` | [`crate::ctx::Ctx::world`] | the lane graph a node has is a *map*, not the world; a map comes through the node's own stores |
//! | `actors()` | [`crate::ctx::Ctx::actors`] | the true set of actors, with true positions — the leak in its purest form |
//! | [`crate::kinematics::Kinematics`] | the mobility provider | ground truth about where the node *is*; the belief is [`PositionEstimate`] |
//! | the simulator's clock | [`crate::ctx::Ctx::now`] | a node knows its own drifting clock, not the true instant |
//! | an [`crate::ids::ActorId`] | everywhere in the engine | it names the body behind a node, which a receiver cannot know |
//!
//! A plug-in that legitimately needs the truth — a mobility model, a perception model, a
//! metric computed for the analyst — takes `Ctx` and says so in its signature. The point is
//! not that ground truth is forbidden; it is that reaching for it is visible in the type.
//!
//! # Shape
//!
//! The types a node's view is *of* live in the crates above this one — the neighbour table
//! in `v2xw-node`, the credential handle in `v2xw-sec`, the verified message in
//! `v2xw-msg` — so they are associated types, exactly as [`crate::ctx::Ctx`] does it for
//! the world and the actor index. A trait object names them:
//! `&dyn NodeView<Neighbors = NeighborTable, Credential = CredentialHandle, Message =
//! VerifiedMessage>`.

use crate::belief::{FixQuality, PositionEstimate};
use crate::ids::NodeId;
use crate::time::SimTime;

/// Everything a plug-in that runs *as a node* may see — and nothing else
/// (invariant I-C2).
///
/// Implemented by the node runtime in `v2xw-node` over its own stores. Passed to
/// `Detector::on_message`, `MessageGenerator::on_tick`, `SafetyApp::on_neighbors` and the
/// attacker's view of itself (03-interfaces.md §6, §9). See the module documentation for
/// what is missing from it and why that is the point.
///
/// Every accessor is `&self` and returns a borrow or a `Copy` scalar, so the trait is
/// dyn-compatible and a view can be handed to several plug-ins at the same instant without
/// any of them being able to mutate it. A plug-in that wants to *act* — transmit, schedule,
/// emit a record, draw a random number — does so through [`crate::ctx::Ctx`], which it is
/// given alongside the view; the split is deliberate, because it keeps "what this node
/// knows" and "what this node does" in different arguments.
pub trait NodeView {
    /// The node's neighbour table: what it has learned about its peers from the messages
    /// it received. `v2xw_node::NeighborTable` in a real engine.
    ///
    /// Note what this is *not*: a list of the actors that are nearby. It contains only
    /// peers this node has heard from, with the positions **they claimed**, at the times
    /// this node received them — including the entries an attacker fabricated.
    type Neighbors;

    /// A credential this node holds, as the security crate models it
    /// (`v2xw_sec::CredentialHandle`): a handle to a certificate and its private key, not
    /// the key material.
    type Credential;

    /// A message this node has received and verified (`v2xw_msg::VerifiedMessage`).
    type Message;

    /// Which node this is.
    ///
    /// The node's own id, which it obviously knows. Not the [`crate::ids::ActorId`] of the
    /// body carrying it: a receiver cannot know that, and a detector that could would be
    /// able to tell two pseudonyms of one vehicle apart for free — which is the very thing
    /// pseudonymity exists to prevent and the very thing a Sybil detector is supposed to
    /// have to *work out*.
    fn node(&self) -> NodeId;

    /// The instant this node **believes** it is, from its own clock.
    ///
    /// Not [`crate::ctx::Ctx::now`]: a node's clock drifts, may have been set from a fix
    /// that is now stale, and may have been stepped by an attacker. Every freshness check a
    /// node performs — a 1609.2 generation-time window, a CAM age, a replay guard — is
    /// performed against *this* number, so a detector that used the simulator's clock would
    /// never see a clock attack at all.
    fn believed_time(&self) -> SimTime;

    /// This node's belief about where it is: position, velocity, heading, error ellipse,
    /// fix quality and the time the fix was valid at.
    ///
    /// The output of its GNSS model, which is ground truth *plus error* — and during an
    /// outage or under spoofing, something much further from the truth than that.
    fn position(&self) -> &PositionEstimate;

    /// The quality of the current fix. Defaulted from [`NodeView::position`].
    fn fix(&self) -> FixQuality {
        self.position().fix
    }

    /// The node's neighbour table.
    fn neighbors(&self) -> &Self::Neighbors;

    /// The credentials this node currently holds, in the order its store keeps them.
    ///
    /// A node's own credentials are node-visible by definition — it enrolled for them and
    /// it signs with them. What is *not* here is any other node's private state, and what
    /// is not derivable from here is which other pseudonyms belong to the same peer.
    fn credentials(&self) -> &[Self::Credential];

    /// The credential this node would sign with now, if it holds one.
    ///
    /// Defaulted to the first entry of [`NodeView::credentials`], which is the convention
    /// the node runtime maintains: the store keeps the currently selected pseudonym first,
    /// and a pseudonym change reorders it. A runtime with a different policy overrides this.
    fn active_credential(&self) -> Option<&Self::Credential> {
        self.credentials().first()
    }

    /// The messages this node has received and verified and still retains, oldest first.
    ///
    /// A bounded buffer — the node's evidence window, sized by its hardware profile — not
    /// the run's message log. This is the raw material of every local detector: what it
    /// actually heard, in the order it heard it, with whatever the senders claimed.
    fn received(&self) -> &[Self::Message];

    /// The most recently received message, if any. Defaulted from [`NodeView::received`].
    fn last_received(&self) -> Option<&Self::Message> {
        self.received().last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::belief::FixQuality;
    use crate::geom::Vec3;
    use crate::kinematics::Kinematics;
    use crate::time::{NS_PER_MS, NS_PER_S};

    /// A neighbour as a node knows one: an id it heard from and the position that peer
    /// *claimed*. Note that there is no true position anywhere in it.
    #[derive(Debug, Clone, PartialEq)]
    struct Neighbor {
        peer: NodeId,
        claimed: Vec3,
        heard_at: SimTime,
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Message {
        from: NodeId,
        claimed_pos: Vec3,
        generation_time: SimTime,
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Credential(u32);

    /// The node runtime's implementation, as `v2xw-node` will write it.
    struct TestNode {
        id: NodeId,
        clock: SimTime,
        belief: PositionEstimate,
        neighbors: Vec<Neighbor>,
        credentials: Vec<Credential>,
        received: Vec<Message>,
    }

    impl NodeView for TestNode {
        type Neighbors = Vec<Neighbor>;
        type Credential = Credential;
        type Message = Message;

        fn node(&self) -> NodeId {
            self.id
        }
        fn believed_time(&self) -> SimTime {
            self.clock
        }
        fn position(&self) -> &PositionEstimate {
            &self.belief
        }
        fn neighbors(&self) -> &Vec<Neighbor> {
            &self.neighbors
        }
        fn credentials(&self) -> &[Credential] {
            &self.credentials
        }
        fn received(&self) -> &[Message] {
            &self.received
        }
    }

    /// Ground truth for the same node, which the detector below never sees.
    fn ground_truth() -> Kinematics {
        Kinematics::at_rest(10 * NS_PER_S, Vec3::new(100.0, 200.0, 0.0))
    }

    fn node() -> TestNode {
        let truth = ground_truth();
        TestNode {
            id: NodeId::new(4),
            // A clock 300 ms fast: the node believes it is later than it is.
            clock: truth.t + 300 * NS_PER_MS,
            belief: PositionEstimate {
                // …and a GNSS error of 3 m east, 4 m north.
                pos: truth.pos + Vec3::new(3.0, 4.0, 0.0),
                semi_major_m: 6.0,
                semi_minor_m: 2.5,
                ..PositionEstimate::perfect(&truth)
            },
            neighbors: vec![Neighbor {
                peer: NodeId::new(9),
                claimed: Vec3::new(400.0, 200.0, 0.0),
                heard_at: truth.t,
            }],
            credentials: vec![Credential(7), Credential(8)],
            received: vec![
                Message {
                    from: NodeId::new(9),
                    claimed_pos: Vec3::new(390.0, 200.0, 0.0),
                    generation_time: truth.t - 100 * NS_PER_MS,
                },
                Message {
                    from: NodeId::new(9),
                    claimed_pos: Vec3::new(400.0, 200.0, 0.0),
                    generation_time: truth.t,
                },
            ],
        }
    }

    /// A detector as a plug-in author writes one: generic over the view, with no way to
    /// reach the world. Everything it computes is a function of belief.
    fn range_plausibility<V>(view: &V, max_range_m: f64) -> Vec<NodeId>
    where
        V: NodeView<Neighbors = Vec<Neighbor>, Credential = Credential, Message = Message>,
    {
        let me = view.position().pos;
        view.received()
            .iter()
            .filter(|m| m.claimed_pos.distance_2d(me) > max_range_m)
            .map(|m| m.from)
            .collect()
    }

    /// The view carries the node's belief, and the belief is *not* the truth: a detector
    /// written against it sees the erroneous position, the drifting clock and the claimed
    /// neighbour positions, which is precisely what a real receiver sees.
    #[test]
    fn a_plugin_sees_belief_and_not_ground_truth() {
        let n = node();
        let truth = ground_truth();

        assert_ne!(
            n.position().pos,
            truth.pos,
            "the belief must be allowed to differ from the truth"
        );
        assert_eq!(n.position().pos.distance_2d(truth.pos), 5.0);
        assert_ne!(n.believed_time(), truth.t, "a node's clock drifts");
        assert_eq!(n.believed_time(), truth.t + 300 * NS_PER_MS);

        // The detector's verdict follows the *belief*: from the believed position the
        // farther claim is 297.03 m away, so a 298 m threshold clears it and a 297 m one
        // does not. Both answers are computed without any access to the truth.
        assert_eq!(range_plausibility(&n, 298.0), Vec::<NodeId>::new());
        assert_eq!(range_plausibility(&n, 297.0), vec![NodeId::new(9)]);
        assert_eq!(range_plausibility(&n, 200.0).len(), 2);

        // And the verdict genuinely differs from the one a truth-reading detector would
        // reach: from the *true* position that same claim is exactly 300 m away, so at a
        // 298 m threshold the honest detector clears it and a cheating one flags it. That
        // gap — a false positive a real receiver would also make, or not make — is the
        // thing a detection-rate metric is supposed to be measuring.
        assert_eq!(
            n.received()[1].claimed_pos.distance_2d(truth.pos),
            300.0,
            "the truth would give a different answer, which is why it is out of reach"
        );
    }

    /// The whole surface, including the defaulted accessors.
    #[test]
    fn the_view_exposes_exactly_the_nodes_own_state() {
        let n = node();
        assert_eq!(n.node(), NodeId::new(4));
        assert_eq!(n.fix(), FixQuality::Rtk);
        assert_eq!(n.fix(), n.position().fix);
        assert_eq!(n.neighbors().len(), 1);
        assert_eq!(n.neighbors()[0].peer, NodeId::new(9));
        assert_eq!(n.neighbors()[0].heard_at, ground_truth().t);
        assert_eq!(n.credentials().len(), 2);
        assert_eq!(n.active_credential(), Some(&Credential(7)));
        assert_eq!(n.received().len(), 2);
        assert_eq!(n.last_received().unwrap().generation_time, ground_truth().t);

        // A node with nothing yet: the defaults degrade to None rather than panicking.
        let empty = TestNode {
            credentials: Vec::new(),
            received: Vec::new(),
            ..node()
        };
        assert_eq!(empty.active_credential(), None);
        assert_eq!(empty.last_received(), None);
        assert_eq!(empty.fix(), FixQuality::Rtk);
    }

    /// Dyn-compatibility, for the same reason [`crate::ctx::Ctx`] needs it: in-process
    /// plug-ins are trait objects, so a family trait method takes `&dyn NodeView<…>`.
    #[test]
    fn node_view_is_usable_as_a_trait_object() {
        fn believed_speed(
            v: &dyn NodeView<Neighbors = Vec<Neighbor>, Credential = Credential, Message = Message>,
        ) -> f64 {
            v.position().ground_speed_mps()
        }

        let n = node();
        let erased: &dyn NodeView<Neighbors = Vec<Neighbor>, Credential = Credential, Message = Message> =
            &n;
        assert_eq!(erased.node(), NodeId::new(4));
        assert_eq!(erased.received().len(), 2);
        assert_eq!(erased.active_credential(), Some(&Credential(7)));
        assert_eq!(believed_speed(erased), 0.0);

        let boxed: Box<
            dyn NodeView<Neighbors = Vec<Neighbor>, Credential = Credential, Message = Message>,
        > = Box::new(node());
        assert_eq!(boxed.believed_time(), ground_truth().t + 300 * NS_PER_MS);
    }
}
