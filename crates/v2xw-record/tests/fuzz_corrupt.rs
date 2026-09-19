//! The promise `error.rs` makes, tested by mutation rather than by example.
//!
//! > Nothing here panics on malformed input: a truncated file, a frame whose `body_len`
//! > lies, a chunk index pointing past the end of the file and a zstd frame that will not
//! > decompress all come back as a named variant. That is a requirement, not a courtesy —
//! > a recording is the artefact a reviewer reaches for *after* something went wrong, so
//! > it is routinely read half-written.
//!
//! It was false. `for_each_message_in_chunk` handed the chunk record's own header to
//! `mcap::read::ChunkReader`, which reserves a buffer of the wire-controlled
//! `uncompressed_size` and then reads each inner record's length out of the *decompressed*
//! bytes and reserves that too. One flipped bit inside a zstd payload therefore produced a
//! multi-exabyte `reserve_exact` and a SIGABRT — which `catch_unwind` cannot trap, so it
//! kills the process and there is no error for a caller to handle — and a chunk header
//! claiming `uncompressed_size = 1` produced an arithmetic overflow. Both were reachable
//! from `verify`, `replay`, `records`, `seek` and `content_digest`, on either source.
//!
//! `tests/corrupt.rs` missed it because it only mutated bytes it computed to be inside the
//! compressed payload and never touched a chunk record's header, and because in the few
//! payload bytes it did touch the CRC happened to fire first.
//!
//! # What each test here claims
//!
//! * [`every_truncation_is_refused`]: every cut point comes back as a named
//!   [`RecordError`], with no exceptions.
//! * [`every_bit_flipped_inside_a_chunk_is_refused`]: every byte of every chunk record,
//!   which is where every message lives. A flip there is refused — or, for the handful of
//!   bits in a zstd frame header that describe the decoder's window rather than the data
//!   and that a conforming decompressor may ignore, it decompresses to *exactly* the
//!   content that was recorded, checked with [`Reader::content_digest`]. What is ruled out
//!   is the thing that matters: no corruption of a chunk is ever decoded into different
//!   data, and none of them aborts.
//! * [`no_mutation_anywhere_in_the_file_panics_or_aborts`]: every byte of the file, on a
//!   stride. A mutation either errors or reads cleanly, and never panics, aborts or
//!   allocates on a number it did not check. It cannot demand an error everywhere: more
//!   than half of this file is schema records carrying the specification's layout tables
//!   as text (§7.1's self-describing requirement), plus an attachment and a library
//!   string, none of which a reader acts on — refusing a file over a flipped bit in a
//!   comment would be wrong, not strict.
//!
//! The counts each test ran are printed, so a regression shows up as a smaller sweep as
//! well as a failure.

use v2xw_record::fixture::{RunShape, scratch_dir, write_recording};
use v2xw_record::{Reader, RecordError};

fn good_recording(tag: &str) -> (Vec<u8>, std::path::PathBuf) {
    let dir = scratch_dir(tag).expect("a scratch directory");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(4, 30)).expect("the recording is written");
    let bytes = std::fs::read(&path).expect("the recording is readable");
    (bytes, path)
}

/// Drives every read path a caller has over one candidate byte string, and returns the
/// digest of everything the recording carries if it read at all.
///
/// All of them, because the defect was in the chunk walk that `verify`, `replay`,
/// `records`, `content_digest` and `seek` all go through, and a fix that covered one entry
/// point would be no fix at all.
fn read_every_path(bytes: Vec<u8>) -> Result<[u8; 32], RecordError> {
    let mut reader = Reader::open_bytes(bytes)?;
    reader.verify()?;
    reader.replay()?;
    reader.records(None)?;
    let digest = reader.content_digest()?;
    if let Some((min, max)) = reader.index().snapshot_span() {
        for t in [min, min + (max - min) / 3, max] {
            reader.seek(t)?;
        }
    }
    Ok(digest)
}

/// Runs one mutation and insists it came back as a named error rather than as a panic, an
/// abort or a silent acceptance.
fn expect_refused(what: &str, bytes: Vec<u8>) -> RecordError {
    match read_every_path(bytes) {
        Ok(_) => panic!("{what}: a corrupt recording was accepted"),
        Err(e) => {
            assert!(!e.to_string().is_empty(), "{what}: the error says nothing");
            e
        }
    }
}

#[test]
fn every_truncation_is_refused() {
    let (bytes, _) = good_recording("fuzz-truncate");
    assert!(bytes.len() > 4_000, "the fixture must be big enough to cut");
    // Every cut point on a 256-step grid, plus every one of the first and last 64 bytes,
    // where the magic, the header record and the footer live.
    let mut cuts: Vec<usize> = (0..256).map(|i| bytes.len() * i / 256).collect();
    cuts.extend(0..64);
    cuts.extend(bytes.len().saturating_sub(64)..bytes.len());
    cuts.sort_unstable();
    cuts.dedup();
    let mut run = 0usize;
    for cut in cuts {
        expect_refused(&format!("cut at {cut}"), bytes[..cut].to_vec());
        run += 1;
    }
    assert!(run >= 300, "only {run} truncations were tried");
    println!("truncations: {run}, every one refused");
}

#[test]
fn every_bit_flipped_inside_a_chunk_is_refused() {
    let (bytes, path) = good_recording("fuzz-chunk");
    let reader = Reader::open(&path).expect("the recording opens");
    let chunks: Vec<(u64, u64)> = reader
        .index()
        .chunks
        .iter()
        .map(|c| (c.file_offset, c.record_length))
        .collect();
    drop(reader);
    assert!(!chunks.is_empty(), "the fixture must contain a chunk");
    let clean = read_every_path(bytes.clone()).expect("the unmutated fixture reads");

    let mut run = 0usize;
    let mut refused = 0usize;
    let mut same_content = 0usize;
    for (offset, length) in &chunks {
        let start = *offset as usize;
        let end = (offset + length) as usize;
        assert!(end <= bytes.len());
        for at in start..end {
            // Bit 0, the smallest possible corruption — and the one the register's own
            // reproduction used (`bytes[457] ^= 0x01`, inside the first chunk's payload).
            let mut broken = bytes.clone();
            broken[at] ^= 0x01;
            run += 1;
            match read_every_path(broken) {
                Err(e) => {
                    assert!(
                        !e.to_string().is_empty(),
                        "byte {at}: an error with no text"
                    );
                    refused += 1;
                }
                Ok(digest) => {
                    assert_eq!(
                        digest, clean,
                        "byte {at} of a chunk was flipped, the recording was accepted, and \
                         what it carries is not what was recorded"
                    );
                    same_content += 1;
                }
            }
        }
    }
    assert!(run > 5_000, "only {run} chunk bytes were flipped");
    assert!(
        same_content * 100 < run,
        "{same_content} of {run} chunk-byte flips changed nothing at all; that is far more \
         than the zstd frame-header bits a decompressor may ignore, and suggests the chunk \
         is not being read"
    );
    println!(
        "chunk-region bit flips: {run} ({refused} refused with a named error, \
         {same_content} decompressed to the identical recorded content)"
    );
}

#[test]
fn the_two_reproductions_from_the_register_are_named_errors() {
    // (1) A single bit flipped inside the first chunk's zstd payload. This is the case that
    // aborted the process: the CRC covers the decompressed records and used to be checked
    // only after the reader had already read a record length out of the corrupt stream and
    // reserved that many bytes. It is checked first now.
    let (bytes, path) = good_recording("fuzz-repro");
    let reader = Reader::open(&path).expect("the recording opens");
    let first = reader.index().chunks[0].clone();
    drop(reader);

    // The compressed payload starts after the record prefix and the chunk header:
    // 9 + (8 start + 8 end + 8 uncompressed_size + 4 crc + 4 compression_len + "zstd" +
    // 8 compressed_size). Derived from the file's own chunk index rather than hard-coded,
    // so the test still aims at the payload if the fixture changes size.
    let payload_at =
        (first.file_offset + 9 + 8 + 8 + 8 + 4 + 4 + first.compression.len() as u64 + 8) as usize;
    let mut flipped = bytes.clone();
    flipped[payload_at + 64] ^= 0x01;
    let err = expect_refused("a bit flipped in the first chunk's zstd payload", flipped);
    let text = err.to_string();
    assert!(
        text.contains("CRC") || text.contains("crc") || text.contains("ecompress"),
        "the error should name the corruption, got: {text}"
    );

    // (2) The chunk record's own `uncompressed_size` set to 1. This overflowed a
    // subtraction in debug and allocated eight exabytes in release; the chunk index holds
    // the authoritative copy, so the two have to agree now.
    let size_at = (first.file_offset + 9 + 8 + 8) as usize;
    let mut lying = bytes.clone();
    lying[size_at..size_at + 8].copy_from_slice(&1u64.to_le_bytes());
    let err = expect_refused("uncompressed_size = 1", lying);
    let text = err.to_string();
    assert!(
        text.contains("uncompressed_size") && text.contains("chunk index"),
        "the error should name the disagreement, got: {text}"
    );

    // …and the same field set past the ceiling, which is the form a reader with no index
    // to cross-check against would still have to survive.
    let mut absurd = bytes.clone();
    absurd[size_at..size_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    expect_refused("uncompressed_size = u64::MAX", absurd);

    // A `compressed_size` that does not add up is the third shape of the same defect: MCAP
    // treats the excess as padding and waits for bytes that are not coming, with an empty
    // input and a non-empty output buffer — a spin rather than an error.
    let compressed_at = size_at + 8 + 4 + 4 + first.compression.len();
    for value in [0u64, 8, u64::MAX, first.record_length] {
        let mut wrong = bytes.clone();
        wrong[compressed_at..compressed_at + 8].copy_from_slice(&value.to_le_bytes());
        expect_refused(&format!("compressed_size = {value}"), wrong);
    }
}

#[test]
fn no_mutation_anywhere_in_the_file_panics_or_aborts() {
    let (bytes, _) = good_recording("fuzz-all");
    let clean = read_every_path(bytes.clone());
    assert!(clean.is_ok(), "the unmutated fixture must read: {clean:?}");

    // Two mutations at each position — a single flipped bit and every bit flipped — over a
    // stride that covers the whole file: the magic, the MCAP header, the metadata record
    // holding the manifest, the chunk, the message indexes, the attachment, the whole
    // summary section and the footer.
    let stride = 11usize;
    let mut run = 0usize;
    let mut errored = 0usize;
    let mut accepted = 0usize;
    for at in (0..bytes.len()).step_by(stride) {
        for mask in [0x01u8, 0xFF] {
            let mut broken = bytes.clone();
            broken[at] ^= mask;
            run += 1;
            match read_every_path(broken) {
                // Named, not a panic: this is the promise.
                Err(e) => {
                    assert!(
                        !e.to_string().is_empty(),
                        "byte {at}: an error with no text"
                    );
                    errored += 1;
                }
                // A byte no reader acts on — a library string, a schema record's layout
                // text, an attachment's payload. Accepting it is correct, and the file
                // still had to be self-consistent to get here, because `read_every_path`
                // ran `verify` and every other read path over it.
                Ok(_) => accepted += 1,
            }
        }
    }
    assert!(run > 3_000, "only {run} mutations were tried");
    assert_eq!(run, errored + accepted);
    assert!(
        errored > run / 4,
        "only {errored} of {run} mutations were detected, which is too few to believe"
    );
    println!(
        "whole-file mutations: {run} ({errored} refused with a named error, \
         {accepted} read cleanly), zero panics and zero aborts"
    );
}
