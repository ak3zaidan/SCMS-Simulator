//! INDEPENDENT VERIFIER INSTRUMENTATION — temporary, removed after the run.
//!
//! Two minimal, deterministic reproductions of defects the independent fuzzing campaign
//! found that survive the F1 repair, plus the silent-content case.

use v2xw_record::fixture::{RunShape, scratch_dir, write_recording};
use v2xw_record::{Reader, RecordError};

fn fixture(tag: &str) -> Vec<u8> {
    let dir = scratch_dir(tag).expect("scratch");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(4, 30)).expect("written");
    std::fs::read(&path).expect("read")
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}
fn put_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn summary_start(b: &[u8]) -> u64 {
    u64_at(b, b.len() - 8 - 20)
}

/// The summary's chunk-index records: `(body_start, body_len)`.
fn chunk_index_records(b: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut p = summary_start(b) as usize;
    let end = b.len() - 8 - 29;
    while p + 9 <= end {
        let op = b[p];
        let len = u64_at(b, p + 1) as usize;
        if p + 9 + len > b.len() {
            break;
        }
        if op == 0x08 {
            out.push((p + 9, len));
        }
        p += 9 + len;
    }
    out
}

/// A: `SeekIndex::read` adds two wire-controlled `u64`s without checking.
///
/// crates/v2xw-record/src/index.rs:369
/// ```ignore
/// if c.file_offset + c.record_length > size {
/// ```
/// Both operands come straight off the summary's chunk-index record. Setting either to
/// `u64::MAX` overflows the add and panics in a debug build — the exact class of defect
/// F1 was raised for, in the module whose documentation says "Nothing here panics on
/// malformed input ... That is a requirement, not a courtesy."
#[test]
fn a_chunk_index_offset_of_u64_max_panics_in_the_index_reader() {
    let clean = fixture("repro-a");
    let idx = chunk_index_records(&clean);
    assert!(!idx.is_empty(), "the fixture has a chunk index");
    let (start, _len) = idx[0];
    for (name, field_off) in [("chunk_start_offset", 16usize), ("chunk_length", 24)] {
        let mut b = clean.clone();
        put_u64(&mut b, start + field_off, u64::MAX);
        let r = std::panic::catch_unwind(move || Reader::open_bytes(b).map(|_| ()));
        match r {
            Err(p) => {
                let m = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_default();
                panic!("DEFECT A CONFIRMED: {name}=u64::MAX panicked with {m:?}");
            }
            Ok(Ok(())) => panic!("{name}=u64::MAX was accepted"),
            Ok(Err(e)) => println!("INDEP-REPRO {name}=u64::MAX -> named error: {e}"),
        }
    }
}

/// B: the `uncompressed_crc = 0` bypass reaches mcap's unchecked subtraction.
///
/// MCAP treats a zero `uncompressed_crc` as "not computed"
/// (mcap-0.25.0/src/sans_io/linear_reader.rs:622), so zeroing those four bytes turns
/// `prevalidate_chunk_crcs` off and hands the corrupt payload to `decompress_inner`,
/// which does `*compressed_remaining -= res.consumed as u64` unchecked at
/// linear_reader.rs:864 — the line the register named in F1.
#[test]
fn zeroing_the_chunk_crc_reopens_the_f1_overflow() {
    let clean = fixture("repro-b");
    let reader = Reader::open_bytes(clean.clone()).expect("opens");
    let (coff, clen) = {
        let c = &reader.index().chunks[0];
        (c.file_offset as usize, c.record_length as usize)
    };
    drop(reader);
    let body = coff + 9;
    let crc_off = body + 24;
    let clen_off = body + 28;
    let compression_len = u32::from_le_bytes(clean[clen_off..clen_off + 4].try_into().unwrap()) as usize;
    let payload = clen_off + 4 + compression_len + 8;
    let payload_end = coff + clen;

    let mut panicked = 0usize;
    let mut different = 0usize;
    let mut errored = 0usize;
    let mut same = 0usize;
    let clean_digest = Reader::open_bytes(clean.clone())
        .and_then(|mut r| r.content_digest())
        .expect("clean digest");
    let mut first_panic = String::new();
    let mut first_diff = String::new();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for i in payload..payload_end {
        let mut b = clean.clone();
        put_u32(&mut b, crc_off, 0);
        b[i] ^= 0x01;
        let label = format!("crc0+bit0@{i}");
        let r = std::panic::catch_unwind(move || -> Result<[u8; 32], RecordError> {
            let mut rd = Reader::open_bytes(b)?;
            rd.verify()?;
            rd.replay()?;
            rd.records(None)?;
            rd.content_digest()
        });
        match r {
            Err(p) => {
                panicked += 1;
                if first_panic.is_empty() {
                    first_panic = format!(
                        "{label}: {:?}",
                        p.downcast_ref::<String>()
                            .cloned()
                            .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    );
                }
            }
            Ok(Err(_)) => errored += 1,
            Ok(Ok(d)) if d == clean_digest => same += 1,
            Ok(Ok(_)) => {
                different += 1;
                if first_diff.is_empty() {
                    first_diff = label;
                }
            }
        }
    }
    std::panic::set_hook(hook);
    println!(
        "INDEP-REPRO crc0 + every single payload bit-0 flip ({} cases): {errored} named errors, \
         {same} clean-and-identical, {different} CLEAN BUT DIFFERENT CONTENT (first {first_diff}), \
         {panicked} PANICS (first {first_panic})",
        payload_end - payload
    );
    assert_eq!(
        panicked, 0,
        "DEFECT B CONFIRMED: the F1 overflow is reachable once the chunk CRC is zero"
    );
    assert_eq!(
        different, 0,
        "DEFECT B' CONFIRMED: corrupt chunks decoded to different content and passed verify()"
    );
}
