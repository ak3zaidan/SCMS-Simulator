//! §10.4 — content. Items C3, C4 and C7.
//!
//! C1 (every telemetry field populated or at its sentinel), C2 (`record_size` striding) and
//! C5 (provenance delivered before it is referenced) are owned by
//! `crates/v2xw-record/tests/conformance.rs`; C6 is the client's `explain` panel.

use v2xw_record::wire::event::{EventBody, EventEntry};
use v2xw_record::wire::{StrTable, get_u32, get_u64};

/// A payload of `len` bytes whose first byte is `tag`, so a mis-strided read is visible.
fn payload(tag: u8, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    v[0] = tag;
    v
}

/// **C3** — "Unknown event `channel_id`s are skipped by `payload_len` without
/// desynchronising."
///
/// The batch carries a known channel, then a channel id this build does not know, then a
/// known one again. A reader that strided by a *guessed* record size would return the third
/// payload shifted; a reader that used `payload_len` returns it intact.
#[test]
fn c3_an_unknown_event_channel_is_skipped_by_payload_len() {
    let at = 4_000_000_000u64;
    let body = EventBody::new(
        at,
        at,
        vec![
            EventEntry {
                sim_time_ns: at,
                channel_id: 10,
                payload: payload(0xA1, 40),
            },
            EventEntry {
                sim_time_ns: at,
                // 0x7F01 is in no v1 channel table: §8.4 adds channel ids in a minor
                // version, and a v1 reader must skip one.
                channel_id: 0x7F01,
                payload: payload(0xB2, 13),
            },
            EventEntry {
                sim_time_ns: at,
                channel_id: 11,
                payload: payload(0xC3, 48),
            },
        ],
    );

    let decoded = EventBody::decode(&body.encode()).expect("the batch decodes");
    assert_eq!(
        decoded.entries.len(),
        3,
        "an unknown channel is not dropped"
    );

    let known: Vec<u16> = decoded.entries.iter().map(|e| e.channel_id).collect();
    assert_eq!(known, vec![10, 11, 0x7F01], "sorted by (time, channel_id)");

    let by_channel = |id: u16| {
        decoded
            .entries
            .iter()
            .find(|e| e.channel_id == id)
            .unwrap_or_else(|| panic!("channel {id} is missing"))
    };
    assert_eq!(by_channel(10).payload[0], 0xA1);
    assert_eq!(by_channel(11).payload[0], 0xC3, "the reader desynchronised");
    assert_eq!(
        by_channel(0x7F01).payload.len(),
        16,
        "the unknown payload is padded to eight and handed back whole, so a reader can \
         stride past it without knowing what it means"
    );
    assert_eq!(by_channel(0x7F01).payload[0], 0xB2);
    assert_eq!(by_channel(10).payload.len(), 40);
}

/// **C4** — "Event index arrays are sorted by `(sim_time_ns, channel_id)` and payloads are
/// 8-aligned."
///
/// The entries go in deliberately out of order, so the encoder is doing the sorting rather
/// than the caller.
#[test]
fn c4_event_entries_are_sorted_and_payloads_are_eight_aligned() {
    let entries = vec![
        EventEntry {
            sim_time_ns: 2_000,
            channel_id: 11,
            payload: payload(0x01, 9),
        },
        EventEntry {
            sim_time_ns: 1_000,
            channel_id: 30,
            payload: payload(0x02, 4),
        },
        EventEntry {
            sim_time_ns: 1_000,
            channel_id: 10,
            payload: payload(0x03, 1),
        },
        EventEntry {
            sim_time_ns: 2_000,
            channel_id: 1,
            payload: payload(0x04, 17),
        },
    ];
    let bytes = EventBody::new(1_000, 2_000, entries).encode();
    let decoded = EventBody::decode(&bytes).expect("the batch decodes");

    let order: Vec<(u64, u16)> = decoded
        .entries
        .iter()
        .map(|e| (e.sim_time_ns, e.channel_id))
        .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted, "the index is not sorted by (time, channel)");
    assert_eq!(
        order,
        vec![(1_000, 10), (1_000, 30), (2_000, 1), (2_000, 11)]
    );

    // The prefix offsets are where the index and the payload area start (§3.6.1).
    assert_eq!(
        get_u64(&bytes, 0, "t_start").expect("prefix"),
        1_000,
        "t_start_ns"
    );
    assert_eq!(get_u64(&bytes, 8, "t_end").expect("prefix"), 2_000);
    assert_eq!(get_u32(&bytes, 16, "count").expect("prefix"), 4);
    let off_payloads = get_u32(&bytes, 24, "off_payloads").expect("prefix") as usize;
    assert_eq!(
        off_payloads % 8,
        0,
        "the payload area starts at {off_payloads}, which is not 8-aligned"
    );
    let payload_bytes = get_u32(&bytes, 28, "payload_bytes").expect("prefix") as usize;
    assert_eq!(
        payload_bytes % 8,
        0,
        "every payload is padded to eight, so the area is a multiple of eight"
    );
    assert_eq!(
        payload_bytes,
        8 + 8 + 16 + 24,
        "1, 4, 9 and 17 bytes pad to 8, 8, 16 and 24"
    );
    assert_eq!(off_payloads + payload_bytes, bytes.len());
}

/// **C7** — "The symbol table is append-only within a connection and resets on a
/// non-resumed `Hello`."
#[test]
fn c7_the_symbol_table_is_append_only_within_a_connection() {
    let mut table = StrTable::new();
    assert_eq!(
        table.get(0),
        Some(""),
        "§2.5: id 0 is always the empty string"
    );

    let first = table.intern("metric/pdr");
    let second = table.intern("metric/ttc");
    assert_ne!(first, second);
    assert_eq!(
        table.intern("metric/pdr"),
        first,
        "interning a string twice must not append a second copy, or two ids would mean \
         one string and a client's cache would be ambiguous"
    );
    assert!(second > first, "ids are assigned in append order");
    assert_eq!(table.get(first), Some("metric/pdr"));
    assert_eq!(table.get(second), Some("metric/ttc"));
    assert_eq!(
        table.get(9_999),
        None,
        "an id past the end resolves to nothing"
    );

    // Nothing ever changes the meaning of an id that has been handed out.
    let before: Vec<String> = table.strings.clone();
    let third = table.intern("0.1.0");
    assert_eq!(
        table.strings[..before.len()],
        before[..],
        "appending must not disturb the ids already in use"
    );
    assert_eq!(table.get(third), Some("0.1.0"));

    // A new connection starts a new table: the ids of the old one mean nothing in it.
    let fresh = StrTable::new();
    assert_eq!(fresh.get(first), None, "a fresh Hello resets the table");
    assert_eq!(fresh.get(0), Some(""));
}
