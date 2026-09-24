//! The VWP v1 conformance checklist as data, and the ledger that says who owns each item.
//!
//! vwp-v1.md §10 is 65 checkboxes. A checklist in prose is a promise; this module turns it
//! into a value, so three things become mechanical:
//!
//! 1. **Nothing drifts.** [`parse`] reads §10 out of the specification at test time and
//!    [`ITEMS`] is what this build believes §10 says. If someone adds a 66th item, renames
//!    `Q3`, or moves an item between sections, the comparison fails and names the
//!    difference, rather than the kit quietly covering 65 of 66.
//! 2. **Nothing is silently uncovered.** [`COVERAGE`] assigns every id an owner, and
//!    `tests/vwp/coverage.rs` checks each owner really exists — a test function in this
//!    kit, a test function in a member crate, or a named id in the TypeScript suite. A
//!    renamed test breaks the ledger instead of quietly orphaning a clause.
//! 3. **Nothing is silently unmet.** An item whose *feature* this build does not implement
//!    is marked [`Entry::unmet`]. The count of unmet items is pinned, so the number can
//!    only go down without an explicit edit.
//!
//! # Why a ledger rather than one test per item everywhere
//!
//! Most of §10's server-side items are already mechanised, well, inside the crate that
//! implements them: `v2xw-record` owns the container items, `v2xw-server` owns the
//! connection items. Re-implementing them here would give two tests that can disagree, and
//! the weaker of the two would be the one that runs against a fixture rather than against
//! the real producer. So the kit mechanises the items that span crates or that nobody owned,
//! and *points at* the rest — which is only worth anything because the pointer is checked.

use std::path::PathBuf;

/// Which side of the protocol an item binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// `S` — the server (this repository's Rust engine and transport).
    Server,
    /// `C` — the client (the TypeScript viewer).
    Client,
    /// `B` — both.
    Both,
}

impl Side {
    /// The single-letter spelling §10 uses.
    #[must_use]
    pub const fn letter(self) -> &'static str {
        match self {
            Side::Server => "S",
            Side::Client => "C",
            Side::Both => "B",
        }
    }

    /// The side a §10 letter names, or `None` for anything else.
    #[must_use]
    pub fn from_letter(s: &str) -> Option<Side> {
        match s {
            "S" => Some(Side::Server),
            "C" => Some(Side::Client),
            "B" => Some(Side::Both),
            _ => None,
        }
    }
}

/// One checklist item as the specification states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedItem {
    /// The id, e.g. `F1`.
    pub id: String,
    /// The side the item binds.
    pub side: Side,
    /// The `### 10.x …` heading it sits under.
    pub section: String,
    /// The item's text, first line only.
    pub text: String,
}

/// Who owns a checklist item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Mechanised in this kit, by the named test function in `tests/vwp/`.
    Here {
        /// The `#[test]` function's name, without its module path.
        test: &'static str,
    },
    /// Mechanised elsewhere, by the named test in the named file.
    ///
    /// `path` is repository-relative. For a Rust file the ledger check looks for
    /// `fn <test>`; for a TypeScript file it looks for the item id, because vitest names
    /// tests with strings rather than identifiers.
    Delegated {
        /// Repository-relative path of the file that owns the item.
        path: &'static str,
        /// The Rust function name, or the TypeScript item id to search for.
        test: &'static str,
    },
    /// Nobody can own it in-process: it is a property of a live socket, a browser, or a
    /// benchmark against wall time.
    Gap {
        /// What would have to exist for the item to be mechanised, in one clause.
        reason: &'static str,
    },
}

/// A row of the coverage ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The checklist id, e.g. `F1`.
    pub id: &'static str,
    /// Who owns it.
    pub coverage: Coverage,
    /// True when this build does **not** satisfy the item, and the owning test pins the
    /// current behaviour so the item goes red the day the feature lands.
    ///
    /// An unmet item is not a failing test: it is a tripwire on a known gap. The gap is
    /// stated in the owning test's documentation and counted by
    /// `tests/vwp/coverage.rs::the_gaps_and_the_unmet_items_are_the_ones_we_know_about`.
    pub unmet: bool,
}

impl Entry {
    /// A met item with an owner in this kit.
    const fn here(id: &'static str, test: &'static str) -> Entry {
        Entry {
            id,
            coverage: Coverage::Here { test },
            unmet: false,
        }
    }

    /// A met item owned by a test elsewhere in the repository.
    const fn there(id: &'static str, path: &'static str, test: &'static str) -> Entry {
        Entry {
            id,
            coverage: Coverage::Delegated { path, test },
            unmet: false,
        }
    }

    /// An item no in-process test can establish.
    const fn gap(id: &'static str, reason: &'static str) -> Entry {
        Entry {
            id,
            coverage: Coverage::Gap { reason },
            unmet: false,
        }
    }

    /// The same as [`Entry::here`], for an item this build does not yet satisfy.
    const fn tripwire(id: &'static str, test: &'static str) -> Entry {
        Entry {
            id,
            coverage: Coverage::Here { test },
            unmet: true,
        }
    }
}

/// How many items §10 has. Pinned so that a specification edit is a visible event.
pub const ITEM_COUNT: usize = 65;

/// How many items this build does not satisfy. Pinned for the same reason.
pub const UNMET_COUNT: usize = 1;

/// How many items no in-process test can establish.
///
/// Pinned so the number can only fall without an explicit edit. Every one of them is a
/// deadline on a live socket, a wall-clock benchmark, or a property of the browser's draw
/// path; each states which in its [`Coverage::Gap`] reason.
pub const GAP_COUNT: usize = 12;

/// The ids §10 lists, in document order, with the side each binds.
///
/// This is what *this build believes* §10 says; [`parse`] is what §10 actually says, and
/// `tests/vwp/coverage.rs` compares them.
pub const ITEMS: &[(&str, Side)] = &[
    // §10.1 Framing
    ("F1", Side::Both),
    ("F2", Side::Client),
    ("F3", Side::Both),
    ("F4", Side::Both),
    ("F5", Side::Both),
    ("F6", Side::Both),
    ("F7", Side::Server),
    ("F8", Side::Both),
    ("F9", Side::Client),
    // §10.2 Handshake, resume, backpressure
    ("H1", Side::Server),
    ("H2", Side::Server),
    ("H3", Side::Client),
    ("H4", Side::Both),
    ("H5", Side::Server),
    ("H6", Side::Server),
    ("H7", Side::Server),
    ("H8", Side::Server),
    ("H9", Side::Server),
    ("H10", Side::Client),
    ("H11", Side::Server),
    // §10.3 Poses and state
    ("Q1", Side::Both),
    ("Q2", Side::Server),
    ("Q3", Side::Server),
    ("Q4", Side::Both),
    ("Q5", Side::Server),
    ("Q6", Side::Both),
    ("Q7", Side::Client),
    // §10.4 Content
    ("C1", Side::Both),
    ("C2", Side::Both),
    ("C3", Side::Both),
    ("C4", Side::Both),
    ("C5", Side::Server),
    ("C6", Side::Client),
    ("C7", Side::Both),
    // §10.5 World
    ("W1", Side::Server),
    ("W2", Side::Server),
    ("W3", Side::Client),
    ("W4", Side::Both),
    ("W5", Side::Server),
    ("W6", Side::Client),
    // §10.6 Visibility
    ("V1", Side::Server),
    ("V2", Side::Server),
    ("V3", Side::Server),
    ("V4", Side::Server),
    ("V5", Side::Server),
    ("V6", Side::Server),
    // §10.7 Control surface
    ("R1", Side::Server),
    ("R2", Side::Server),
    ("R3", Side::Server),
    ("R4", Side::Server),
    ("R5", Side::Server),
    ("R6", Side::Server),
    ("R7", Side::Server),
    ("R8", Side::Server),
    ("R9", Side::Client),
    ("R10", Side::Server),
    // §10.8 Replay
    ("P1", Side::Both),
    ("P2", Side::Server),
    ("P3", Side::Server),
    ("P4", Side::Server),
    ("P5", Side::Server),
    ("P6", Side::Server),
    // §10.9 Versioning
    ("N1", Side::Both),
    ("N2", Side::Server),
    ("N3", Side::Client),
];

/// Who owns each item, in the same order as [`ITEMS`].
pub const COVERAGE: &[Entry] = &[
    // --- §10.1 Framing -----------------------------------------------------------------
    Entry::here("F1", "f1_a_frame_whose_magic_is_wrong_is_refused"),
    Entry::here("F2", "f2_an_unknown_message_type_is_ignored_rather_than_an_error"),
    Entry::here("F3", "f3_body_len_is_the_uncompressed_body_length"),
    Entry::here("F4", "f4_every_scalar_on_the_wire_is_little_endian"),
    Entry::here("F5", "f5_every_array_offset_satisfies_the_alignment_rule"),
    Entry::here("F6", "f6_reserved_bytes_are_written_zero_and_ignored_on_read"),
    Entry::here("F7", "f7_hello_is_never_compressed"),
    Entry::tripwire("F8", "f8_compression_is_not_applied_on_the_wire_in_this_build"),
    Entry::there(
        "F9",
        "ui/packages/protocol/test/framing.test.ts",
        "F9",
    ),
    // --- §10.2 Handshake, resume, backpressure ------------------------------------------
    Entry::gap(
        "H1",
        "`Hello` within 1000 ms of the upgrade is a wall-clock property of the axum socket task",
    ),
    Entry::there(
        "H2",
        "crates/v2xw-server/tests/session.rs",
        "the_first_canonical_frame_after_a_fresh_hello_is_a_resync_keyframe",
    ),
    Entry::there("H3", "ui/packages/protocol/test/client.test.ts", "H3"),
    Entry::here("H4", "h4_seq_is_dense_and_monotonic_and_hello_carries_the_next_one"),
    Entry::here("H5", "h5_a_resume_point_inside_the_ring_replays_without_a_gap"),
    Entry::there(
        "H6",
        "crates/v2xw-server/tests/session.rs",
        "a_resume_the_ring_cannot_serve_falls_back_rather_than_failing",
    ),
    Entry::there(
        "H7",
        "crates/v2xw-server/tests/framing.rs",
        "queue_bytes_never_exceed_the_cap_under_a_client_that_reads_nothing",
    ),
    Entry::gap(
        "H8",
        "`resync_deadline_ms` is measured against wall time on a live connection",
    ),
    Entry::there(
        "H9",
        "crates/v2xw-server/tests/framing.rs",
        "deltas_are_dropped_all_or_nothing_and_ask_for_a_resync",
    ),
    Entry::there("H10", "ui/packages/protocol/test/client.test.ts", "H10"),
    Entry::gap(
        "H11",
        "ping every 15 s and close 1001 after 30 s are wall-clock properties of the socket task",
    ),
    // --- §10.3 Poses and state -----------------------------------------------------------
    Entry::here("Q1", "q1_quantisation_matches_the_normative_rules"),
    Entry::there(
        "Q2",
        "crates/v2xw-record/tests/drift.rs",
        "delta_quantisation_does_not_drift_over_ten_thousand_steps",
    ),
    Entry::there(
        "Q3",
        "crates/v2xw-record/tests/drift.rs",
        "a_teleport_larger_than_the_delta_range_uses_the_absolute_escape",
    ),
    Entry::here("Q4", "q4_keyframe_actor_rows_are_indexed_by_slot"),
    Entry::there(
        "Q5",
        "crates/v2xw-record/tests/conformance.rs",
        "a_slot_is_not_reused_until_a_keyframe_period_after_its_despawn",
    ),
    Entry::here("Q6", "q6_the_state_byte_bits_and_the_meaning_of_benign"),
    Entry::gap(
        "Q7",
        "rendering with an absent lane id or a zero acceleration is a property of the viewer's draw path",
    ),
    // --- §10.4 Content --------------------------------------------------------------------
    Entry::there(
        "C1",
        "crates/v2xw-record/tests/conformance.rs",
        "every_telemetry_field_round_trips_and_has_a_sentinel",
    ),
    Entry::there(
        "C2",
        "crates/v2xw-record/tests/conformance.rs",
        "a_synthetic_v1_1_stream_parses_and_loses_only_what_is_new",
    ),
    Entry::here("C3", "c3_an_unknown_event_channel_is_skipped_by_payload_len"),
    Entry::here("C4", "c4_event_entries_are_sorted_and_payloads_are_eight_aligned"),
    Entry::there(
        "C5",
        "crates/v2xw-record/tests/conformance.rs",
        "every_referenced_provenance_id_was_delivered_before_it_was_referenced",
    ),
    Entry::gap(
        "C6",
        "`explain` resolving for every displayed value is a property of the viewer's why panel",
    ),
    Entry::here("C7", "c7_the_symbol_table_is_append_only_within_a_connection"),
    // --- §10.5 World ------------------------------------------------------------------------
    Entry::here("W1", "w1_the_payload_url_hash_is_the_digest_of_the_body"),
    Entry::gap(
        "W2",
        "COOP/COEP/CORP headers are set by the axum router and need a live HTTP response",
    ),
    Entry::there("W3", "ui/packages/protocol/test/world.test.ts", "W3"),
    Entry::here("W4", "w4_world_json_binary_parity"),
    Entry::here("W5", "w5_world_generate_is_a_pure_function_of_its_parameters"),
    Entry::gap(
        "W6",
        "handling `world_ref.mode` 1 and 2 is a property of the viewer's world loader",
    ),
    // --- §10.6 Visibility ---------------------------------------------------------------------
    Entry::there(
        "V1",
        "crates/v2xw-record/tests/node_profile.rs",
        "every_ground_truth_field_of_the_exhaustive_list_is_blanked",
    ),
    Entry::here("V2", "v2_an_unequipped_actor_occupies_an_empty_slot_in_the_node_profile"),
    Entry::here("V3", "v3_a_ground_truth_channel_metric_or_overlay_is_refused_with_32040"),
    Entry::here("V4", "v4_no_control_method_can_change_the_profile"),
    Entry::there(
        "V5",
        "crates/v2xw-record/tests/node_profile.rs",
        "stripping_agrees_with_a_live_node_profile_producer",
    ),
    Entry::there(
        "V6",
        "crates/v2xw-record/tests/export.rs",
        "the_node_only_export_contains_no_ground_truth",
    ),
    // --- §10.7 Control surface -----------------------------------------------------------------
    Entry::here("R1", "r1_all_thirty_three_methods_are_implemented_and_discoverable"),
    Entry::there(
        "R2",
        "crates/v2xw-server/tests/rpc.rs",
        "every_schema_ref_in_the_document_resolves",
    ),
    Entry::there(
        "R3",
        "crates/v2xw-server/tests/rpc.rs",
        "invalid_params_data_is_an_array_of_path_message_hint",
    ),
    Entry::gap(
        "R4",
        "`run.seek` sending its frames before the reply needs a dispatched call over a live session",
    ),
    Entry::gap(
        "R5",
        "`run.pause` having sent every frame up to the reply's t_ns needs a dispatched call over a live session",
    ),
    Entry::gap(
        "R6",
        "`run.step {unit:\"event\"}` advancing exactly one DES event needs a running kernel behind the RPC layer",
    ),
    Entry::here("R7", "r7_connection_scoped_methods_are_refused_on_the_http_path"),
    Entry::gap(
        "R8",
        "the 2 s job threshold is measured against wall time on a live connection",
    ),
    Entry::there("R9", "ui/packages/protocol/test/client.test.ts", "R9"),
    Entry::here("R10", "r10_a_json_rpc_batch_array_is_refused_with_32600"),
    // --- §10.8 Replay -------------------------------------------------------------------------
    Entry::there(
        "P1",
        "crates/v2xw-record/tests/byte_identity.rs",
        "a_replayed_stream_is_byte_identical_to_the_live_one",
    ),
    Entry::gap(
        "P2",
        "`bench_seek`'s p95 and max are wall-clock measurements and belong in a benchmark, not a test",
    ),
    Entry::there(
        "P3",
        "crates/v2xw-record/tests/byte_identity.rs",
        "a_replayed_stream_is_byte_identical_to_the_live_one",
    ),
    Entry::there(
        "P4",
        "crates/v2xw-record/tests/seek.rs",
        "seeking_lands_on_the_preceding_keyframe_from_many_targets",
    ),
    Entry::there(
        "P5",
        "crates/v2xw-wasm/tests/replay.rs",
        "the_session_hands_back_the_recorded_frames",
    ),
    Entry::there(
        "P6",
        "crates/v2xw-record/tests/conformance.rs",
        "a_recording_from_a_future_major_is_refused_and_a_future_minor_is_accepted",
    ),
    // --- §10.9 Versioning ----------------------------------------------------------------------
    Entry::there(
        "N1",
        "crates/v2xw-record/tests/conformance.rs",
        "a_synthetic_v1_1_stream_parses_and_loses_only_what_is_new",
    ),
    Entry::here("N2", "n2_the_subprotocol_token_is_vwp_v1"),
    Entry::there("N3", "ui/packages/protocol/test/framing.test.ts", "N3"),
];

/// The ledger row for an id, if there is one.
#[must_use]
pub fn entry(id: &str) -> Option<&'static Entry> {
    COVERAGE.iter().find(|e| e.id == id)
}

/// The path of the specification the checklist lives in.
#[must_use]
pub fn spec_path() -> PathBuf {
    crate::repo_root().join("docs/protocol/vwp-v1.md")
}

/// Reads §10 out of `markdown` and returns the items it states, in document order.
///
/// The parser is deliberately literal: a line that begins `- [ ]` or `- [x]` and whose
/// first bold run is `<letter> · <id>` is an item, and everything after that run up to the
/// end of the line is its text. A specification edit that changes the shape of a row makes
/// the parse return fewer items, which `tests/vwp/coverage.rs` reports as a count mismatch
/// rather than absorbing.
#[must_use]
pub fn parse(markdown: &str) -> Vec<ParsedItem> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut inside = false;

    for raw in markdown.lines() {
        let line = raw.trim();
        if line.starts_with("## ") {
            inside = line.starts_with("## 10. ");
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            section = rest.trim().to_string();
            continue;
        }
        let Some(rest) = line
            .strip_prefix("- [ ] ")
            .or_else(|| line.strip_prefix("- [x] "))
        else {
            continue;
        };
        let Some(after_open) = rest.strip_prefix("**") else {
            continue;
        };
        let Some(close) = after_open.find("**") else {
            continue;
        };
        let label = &after_open[..close];
        let text = after_open[close + 2..].trim().to_string();
        let mut parts = label.split('\u{b7}');
        let (Some(side_text), Some(id_text)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Some(side) = Side::from_letter(side_text.trim()) else {
            continue;
        };
        out.push(ParsedItem {
            id: id_text.trim().to_string(),
            side,
            section: section.clone(),
            text,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parser reads the shape §10 actually uses, including a continuation line and an
    /// already-ticked box, and stops at the appendix.
    ///
    /// Synthetic input, so the parser is tested against the grammar rather than against the
    /// document it will be pointed at — which is the difference between a parser test and a
    /// tautology.
    #[test]
    fn the_parser_reads_the_shape_the_specification_uses() {
        let md = "## 9. Worked example\n\
                  - [ ] **B · X9** not in section ten\n\
                  ## 10. Conformance checklist\n\
                  \n\
                  ### 10.1 Framing\n\
                  \n\
                  - [ ] **B · F1** Rejects a frame whose `magic` is wrong.\n\
                  - [x] **S · F7** `Hello` is never compressed.\n\
                  - [ ] **C · F9** Unknown flag bits are ignored, not treated as errors,\n\
                  \x20     and the connection survives.\n\
                  \n\
                  ## Appendix A — Enum reference\n\
                  - [ ] **B · Z1** not in section ten either\n";
        let items = parse(md);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["F1", "F7", "F9"], "parsed {items:?}");
        assert_eq!(items[0].side, Side::Both);
        assert_eq!(items[1].side, Side::Server);
        assert_eq!(items[2].side, Side::Client);
        assert!(items.iter().all(|i| i.section == "10.1 Framing"));
        assert!(items[0].text.starts_with("Rejects a frame"));
    }

    /// The injected fault for the parser: break one row's shape and it must be reported as
    /// missing rather than absorbed.
    #[test]
    fn a_row_whose_shape_changed_is_lost_rather_than_guessed_at() {
        let md = "## 10. Conformance checklist\n\
                  ### 10.1 Framing\n\
                  - [ ] **B - F1** the middle dot became a hyphen\n\
                  - [ ] **B · F2** intact\n";
        let ids: Vec<String> = parse(md).into_iter().map(|i| i.id).collect();
        assert_eq!(ids, vec!["F2".to_string()]);
    }

    /// Every id in the ledger is unique, and the ledger has one row per listed item.
    ///
    /// This is a property of the two constants in this file and needs no I/O, so it is a
    /// unit test; the comparison against the *document* needs the document and lives in
    /// `tests/vwp/coverage.rs`.
    #[test]
    fn the_ledger_covers_the_item_list_exactly_once_each() {
        assert_eq!(ITEMS.len(), ITEM_COUNT);
        assert_eq!(COVERAGE.len(), ITEM_COUNT);
        for (id, _) in ITEMS {
            let rows = COVERAGE.iter().filter(|e| e.id == *id).count();
            assert_eq!(rows, 1, "{id} appears {rows} times in the ledger");
        }
        assert_eq!(
            COVERAGE.iter().filter(|e| e.unmet).count(),
            UNMET_COUNT,
            "the number of items this build does not satisfy has changed"
        );
    }
}
