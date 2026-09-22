//! §10.6 — visibility and the `NODE-only` profile. Items V2, V3 and V4.
//!
//! V1 (field-by-field leakage), V5 (stripped and live blind producers agree) and V6 (the
//! exporters' leakage linter) are owned by `crates/v2xw-record/tests/node_profile.rs` and
//! `crates/v2xw-record/tests/export.rs`, which run against the real stripper rather than
//! against a fixture of the kit's own.

use v2xw_record::fixture::{RunShape, live_frames};
use v2xw_record::profile::Profile;
use v2xw_record::wire::snapshot::{ActorRow, KeyframeBody, PROFILE_NODE, ST_EQUIPPED};
use v2xw_record::wire::{FLAG_NODE_ONLY, MsgType};
use v2xw_server::error::ServerError;
use v2xw_server::rpc::METHODS;
use v2xw_server::session::{ConnectParams, OVERLAYS, overlay_is_gt};

/// The first keyframe of a run in `profile`, decoded.
fn first_keyframe(profile: Profile) -> KeyframeBody {
    let shape = RunShape {
        actors: 6,
        steps: 12,
        profile,
        ..RunShape::default()
    };
    let frames = live_frames(&shape).expect("the fixture encodes");
    let frame = frames
        .iter()
        .find(|f| {
            f.header()
                .is_ok_and(|h| h.kind() == Some(MsgType::Keyframe))
        })
        .expect("the run opens with a keyframe");
    if profile.is_node_only() {
        assert_eq!(
            frame.header().expect("a header").flags & FLAG_NODE_ONLY,
            FLAG_NODE_ONLY,
            "§2.3: a node-profile frame carries FLAG_NODE_ONLY"
        );
    }
    KeyframeBody::decode(frame.body()).expect("the keyframe decodes")
}

/// **V2** — "In `profile=node`, actors without `ST_EQUIPPED` occupy empty slots."
///
/// The fixture's actor 1 is an unequipped pedestrian, so the blind stream has something to
/// withhold. The full-profile run is the control: if both profiles showed the same number
/// of occupied slots, the blanking would not be happening and the assertion below would be
/// about nothing.
#[test]
fn v2_an_unequipped_actor_occupies_an_empty_slot_in_the_node_profile() {
    let blind = first_keyframe(Profile::NodeOnly);
    assert_eq!(blind.profile, PROFILE_NODE, "§3.3.1's profile word");

    let occupied: Vec<&ActorRow> = blind.actors.iter().filter(|r| r.is_occupied()).collect();
    assert!(!occupied.is_empty(), "the blind stream shows nobody at all");
    for row in &occupied {
        assert_ne!(
            row.state & ST_EQUIPPED,
            0,
            "an actor without ST_EQUIPPED reached the blind stream: {row:?}"
        );
    }
    let empties = blind.actors.len() - occupied.len();
    assert!(
        empties > 0,
        "every slot is occupied, so no unequipped actor was withheld and V2 checked \
         nothing"
    );

    let full = first_keyframe(Profile::Full);
    let full_occupied = full.actors.iter().filter(|r| r.is_occupied()).count();
    assert!(
        full_occupied > occupied.len(),
        "the full profile shows {full_occupied} actors and the node profile {}, so the \
         profile is not withholding anything",
        occupied.len()
    );
}

/// **V3** — "`events.set` on a GT channel, `metrics.query` on a GT metric, and
/// `overlay.set` on a `*_gt` overlay all return `-32040`."
///
/// The two halves that need no dispatched call: the error the three share carries the code
/// §6.4 assigns it and names the field it withheld, and the predicate that decides which
/// overlays are ground truth agrees with the published overlay list. The dispatch half —
/// that each of the three methods actually raises it — is owned by
/// `crates/v2xw-server/tests/session.rs`.
#[test]
fn v3_a_ground_truth_channel_metric_or_overlay_is_refused_with_32040() {
    let denied = ServerError::VisibilityDenied {
        field: "gt.kinematics".to_string(),
        visibility: "GT",
    };
    assert_eq!(denied.code(), -32040, "§6.4 assigns -32040");
    let object = denied.to_rpc_object();
    assert_eq!(object["code"], -32040);
    assert!(
        object["message"]
            .as_str()
            .is_some_and(|m| m.contains("gt.kinematics")),
        "the refusal must name what it withheld: {object}"
    );

    let gt: Vec<&str> = OVERLAYS
        .iter()
        .copied()
        .filter(|name| overlay_is_gt(name))
        .collect();
    let ordinary: Vec<&str> = OVERLAYS
        .iter()
        .copied()
        .filter(|name| !overlay_is_gt(name))
        .collect();
    assert!(
        !gt.is_empty(),
        "no overlay is ground truth, so the refusal can never fire"
    );
    assert!(
        !ordinary.is_empty(),
        "every overlay is ground truth, so the node profile can render nothing"
    );
    for name in &gt {
        assert!(name.ends_with("_gt"), "{name} is classified GT but is not named so");
    }
    assert!(gt.contains(&"attackers_gt"), "{gt:?}");
    assert!(ordinary.contains(&"links"), "{ordinary:?}");
}

/// **V4** — "The profile cannot be changed by any control method."
///
/// The profile is fixed by the connection's query string and appears in no method name and
/// in no method's contract. Checked against the published inventory rather than against the
/// dispatcher, because a method that existed but was unlisted would break R1 instead.
#[test]
fn v4_no_control_method_can_change_the_profile() {
    for method in METHODS {
        assert!(
            !method.contains("profile"),
            "`{method}` names the profile, so §5.3's 'the profile cannot be changed' \
             needs re-checking"
        );
    }

    // It is set once, at connect, and a value the server does not know is refused rather
    // than silently becoming `full` — which is the failure that would hand a blind client
    // the ground truth.
    assert_eq!(
        ConnectParams::parse("profile=node")
            .expect("profile=node parses")
            .profile,
        Profile::NodeOnly
    );
    assert_eq!(
        ConnectParams::parse("").expect("the default parses").profile,
        Profile::Full,
        "§1.1: profile defaults to full"
    );
    assert_eq!(
        ConnectParams::parse("profile=gods-eye")
            .expect_err("an unknown profile is refused")
            .code(),
        -32602,
        "an unknown profile must never silently become `full`"
    );
}
