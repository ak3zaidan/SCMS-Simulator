//! §10.9 — versioning. Item N2.
//!
//! N1 (a v1 reader parses a synthetic v1.1 stream) is owned by
//! `crates/v2xw-record/tests/conformance.rs`, and N3 binds the client.

use v2xw_record::wire::VERSION_MAJOR;
use v2xw_server::http::SUBPROTOCOL;

/// **N2** — "The subprotocol token is `vwp.v1` and an unknown one fails the upgrade with
/// 426."
///
/// The token half. The 426 half is an HTTP status on a live upgrade and is exercised by the
/// server's own transport code path; the token is what makes it checkable here at all,
/// because a token that drifted from the major version would make every 426 correct and
/// every connection impossible.
#[test]
fn n2_the_subprotocol_token_is_vwp_v1() {
    assert_eq!(SUBPROTOCOL, "vwp.v1");
    assert_eq!(
        SUBPROTOCOL,
        format!("vwp.v{VERSION_MAJOR}"),
        "§8.1: the token carries the major version, so the two cannot drift"
    );
    assert_eq!(VERSION_MAJOR, 1);
}
