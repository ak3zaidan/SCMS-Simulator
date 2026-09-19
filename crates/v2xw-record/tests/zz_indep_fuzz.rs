//! INDEPENDENT VERIFIER INSTRUMENTATION — temporary, removed after the run.
//!
//! An adversarial re-derivation of the F1 claim, written without reading
//! `tests/fuzz_corrupt.rs`'s strategy. It differs from that file on purpose:
//!
//!  * a MULTI-CHUNK recording as well as a single-chunk one, so the chunk index
//!    carries several entries and the message indexes are non-trivial;
//!  * mutation kinds beyond a bit flip: byte substitution, runs of inverted bytes,
//!    pseudo-random multi-byte smashes;
//!  * STRUCTURED attacks that know the MCAP layout — the footer's `summary_start`,
//!    the chunk record's `uncompressed_size` / `uncompressed_crc` / `compressed_size`,
//!    the summary chunk-index record's copies of the same numbers, the message-index
//!    offsets, the attachment and the metadata record lengths;
//!  * the COORDINATED attack the F1 fix is most exposed to: change the chunk record's
//!    `uncompressed_size` and the summary chunk index's copy of it to the SAME wrong
//!    value, so the cross-check agrees and the number still reaches an allocator;
//!  * the `uncompressed_crc = 0` bypass: MCAP treats zero as "CRC not computed", so
//!    zeroing it turns off `prevalidate_chunk_crcs` and lets a corrupt zstd payload
//!    through to the decompressor;
//!  * every case is driven through the in-memory reader AND the file readers
//!    (`Reader::open`, `Reader::open_paged`), because the file path is a different
//!    `Source`;
//!  * every case is bounded by a watchdog thread, so a hang is a failure with a
//!    distinctive exit code rather than a test that never returns.

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use v2xw_record::fixture::{RunShape, scratch_dir, write_recording, write_recording_with};
use v2xw_record::writer::RecordingOptions;
use v2xw_record::{Reader, RecordError};

// ---------------------------------------------------------------- watchdog

static CASE: AtomicU64 = AtomicU64::new(0);
static LABEL: Mutex<String> = Mutex::new(String::new());
static START: Mutex<Option<Instant>> = Mutex::new(None);
static WATCHDOG: std::sync::Once = std::sync::Once::new();

/// Every case must finish inside this; anything slower counts as a hang.
const CASE_BUDGET: Duration = Duration::from_secs(20);

fn start_watchdog() {
    WATCHDOG.call_once(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let started = *START.lock().unwrap();
                if let Some(t0) = started {
                    if t0.elapsed() > CASE_BUDGET {
                        let label = LABEL.lock().unwrap().clone();
                        eprintln!(
                            "INDEP-FUZZ HANG: case {} ({label}) exceeded {:?}",
                            CASE.load(Ordering::Relaxed),
                            CASE_BUDGET
                        );
                        let _ = std::io::stderr().flush();
                        std::process::exit(97);
                    }
                }
            }
        });
    });
}

fn case_begin(label: &str) {
    start_watchdog();
    CASE.fetch_add(1, Ordering::Relaxed);
    *LABEL.lock().unwrap() = label.to_string();
    *START.lock().unwrap() = Some(Instant::now());
}

fn case_end() -> Duration {
    let d = START.lock().unwrap().take().map_or(Duration::ZERO, |t| t.elapsed());
    d
}

// ---------------------------------------------------------------- outcomes

#[derive(Debug)]
enum Outcome {
    /// Read cleanly, and this is the digest of everything it carried.
    Clean([u8; 32]),
    /// A named `RecordError`.
    Named(String),
    /// A panic — a defect.
    Panicked(String),
}

fn read_every_path_from_bytes(bytes: Vec<u8>) -> Result<[u8; 32], RecordError> {
    let mut reader = Reader::open_bytes(bytes)?;
    reader.verify()?;
    reader.replay()?;
    reader.records(None)?;
    let digest = reader.content_digest()?;
    if let Some((min, max)) = reader.index().snapshot_span() {
        for t in [min, min + (max - min) / 3, min + (max - min) / 2, max] {
            reader.seek(t)?;
        }
    }
    Ok(digest)
}

fn read_every_path_from_file(path: &std::path::Path) -> Result<[u8; 32], RecordError> {
    let mut resident = Reader::open(path)?;
    resident.verify()?;
    resident.replay()?;
    resident.records(None)?;
    let digest = resident.content_digest()?;
    let mut paged = Reader::open_paged(path)?;
    paged.verify()?;
    if let Some((min, max)) = paged.index().snapshot_span() {
        for t in [min, (min + max) / 2, max] {
            paged.seek(t)?;
        }
    }
    Ok(digest)
}

/// Runs one candidate through every read path, on both sources, catching panics.
fn run_case(label: &str, bytes: &[u8], scratch: &std::path::Path) -> Outcome {
    case_begin(label);
    let owned = bytes.to_vec();
    let p = scratch.to_path_buf();
    let r = std::panic::catch_unwind(move || {
        let mem = read_every_path_from_bytes(owned.clone());
        std::fs::write(&p, &owned).expect("scratch write");
        let file = read_every_path_from_file(&p);
        match (mem, file) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a, b, "the memory and file sources disagreed on the content");
                Ok(a)
            }
            (Err(e), Err(_)) => Err(e.to_string()),
            (Ok(_), Err(e)) => Err(format!("file source only: {e}")),
            (Err(e), Ok(_)) => Err(format!("memory source only: {e}")),
        }
    });
    let elapsed = case_end();
    assert!(
        elapsed < CASE_BUDGET,
        "{label} took {elapsed:?}, past the budget"
    );
    match r {
        Ok(Ok(d)) => Outcome::Clean(d),
        Ok(Err(e)) => Outcome::Named(e),
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "<non-string panic>".to_string());
            Outcome::Panicked(msg)
        }
    }
}

// ---------------------------------------------------------------- fixtures

struct Fixture {
    bytes: Vec<u8>,
    scratch: std::path::PathBuf,
    clean: [u8; 32],
    chunks: Vec<(u64, u64)>,
}

fn build(tag: &str, shape: RunShape, chunk_target: u64) -> Fixture {
    let dir = scratch_dir(tag).expect("scratch");
    let path = dir.join("run.mcap");
    write_recording_with(
        &path,
        &shape,
        RecordingOptions {
            cadence: shape.cadence,
            profile: shape.profile,
            chunk_target_bytes: chunk_target,
            ..Default::default()
        },
        true,
    )
    .expect("recording written");
    let bytes = std::fs::read(&path).expect("read back");
    let reader = Reader::open(&path).expect("opens");
    let chunks: Vec<(u64, u64)> = reader
        .index()
        .chunks
        .iter()
        .map(|c| (c.file_offset, c.record_length))
        .collect();
    drop(reader);
    let clean = read_every_path_from_bytes(bytes.clone()).expect("the clean fixture reads");
    Fixture {
        bytes,
        scratch: dir.join("mutant.mcap"),
        clean,
        chunks,
    }
}

// ---------------------------------------------------------- layout helpers

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}
fn put_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

/// `summary_start` from the footer record (op 0x02, 20-byte body, then 8 magic bytes).
fn summary_start(b: &[u8]) -> u64 {
    u64_at(b, b.len() - 8 - 20)
}
fn summary_start_offset(b: &[u8]) -> usize {
    b.len() - 8 - 20
}

/// Walks the summary section, returning `(opcode, body_start, body_len)` per record.
fn summary_records(b: &[u8]) -> Vec<(u8, usize, usize)> {
    let mut out = Vec::new();
    let mut p = summary_start(b) as usize;
    let end = b.len() - 8 - 29;
    while p + 9 <= end {
        let op = b[p];
        let len = u64_at(b, p + 1) as usize;
        if p + 9 + len > b.len() {
            break;
        }
        out.push((op, p + 9, len));
        p += 9 + len;
    }
    out
}

/// The summary's chunk-index records: `(body_start, body_len)`.
fn chunk_index_records(b: &[u8]) -> Vec<(usize, usize)> {
    summary_records(b)
        .into_iter()
        .filter(|(op, _, _)| *op == 0x08)
        .map(|(_, s, l)| (s, l))
        .collect()
}

/// Field offsets inside a CHUNK record's body (op 0x06):
/// `start u64 | end u64 | uncompressed_size u64 | uncompressed_crc u32 |
///  compression_len u32 | compression | compressed_size u64 | data`.
const CHUNK_UNCOMPRESSED_SIZE: usize = 16;
const CHUNK_CRC: usize = 24;
const CHUNK_COMPRESSION_LEN: usize = 28;

fn chunk_body(b: &[u8], file_offset: u64) -> usize {
    file_offset as usize + 9
}
fn chunk_compressed_size_offset(b: &[u8], file_offset: u64) -> usize {
    let body = chunk_body(b, file_offset);
    let clen = u32::from_le_bytes(
        b[body + CHUNK_COMPRESSION_LEN..body + CHUNK_COMPRESSION_LEN + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    body + CHUNK_COMPRESSION_LEN + 4 + clen
}
fn chunk_payload_offset(b: &[u8], file_offset: u64) -> usize {
    chunk_compressed_size_offset(b, file_offset) + 8
}

// ------------------------------------------------------------------- LCG

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// ------------------------------------------------------------------ tests

struct Tally {
    named: usize,
    clean: usize,
    panicked: Vec<String>,
}
impl Tally {
    fn new() -> Self {
        Tally {
            named: 0,
            clean: 0,
            panicked: Vec::new(),
        }
    }
    fn add(&mut self, label: &str, o: Outcome) -> Option<[u8; 32]> {
        match o {
            Outcome::Named(_) => {
                self.named += 1;
                None
            }
            Outcome::Clean(d) => {
                self.clean += 1;
                Some(d)
            }
            Outcome::Panicked(m) => {
                self.panicked.push(format!("{label}: {m}"));
                None
            }
        }
    }
    fn report(&self, what: &str) {
        println!(
            "INDEP-FUZZ {what}: {} cases, {} named errors, {} clean reads, {} panics",
            self.named + self.clean + self.panicked.len(),
            self.named,
            self.clean,
            self.panicked.len()
        );
        assert!(
            self.panicked.is_empty(),
            "{what}: {} panics, first: {}",
            self.panicked.len(),
            self.panicked[0]
        );
    }
}

#[test]
fn indep_every_truncation_is_a_named_error() {
    let f = build("indep-trunc", RunShape::new(6, 60), 16 * 1024);
    println!(
        "INDEP-FUZZ fixture: {} bytes, {} chunks",
        f.bytes.len(),
        f.chunks.len()
    );
    assert!(f.chunks.len() >= 3, "wanted a multi-chunk fixture");
    let n = f.bytes.len();
    let mut cuts: Vec<usize> = (0..512).map(|i| n * i / 512).collect();
    cuts.extend(0..96);
    cuts.extend(n.saturating_sub(96)..n);
    for (off, len) in &f.chunks {
        cuts.push(*off as usize);
        cuts.push(*off as usize + 9);
        cuts.push(*off as usize + (*len as usize) / 2);
        cuts.push((*off + *len) as usize);
    }
    cuts.push(summary_start(&f.bytes) as usize);
    cuts.sort_unstable();
    cuts.dedup();
    let mut t = Tally::new();
    for cut in &cuts {
        let label = format!("truncate@{cut}");
        let o = run_case(&label, &f.bytes[..*cut], &f.scratch);
        if let Outcome::Clean(_) = o {
            panic!("{label}: a truncated recording read cleanly");
        }
        t.add(&label, o);
    }
    t.report("truncation");
    assert!(cuts.len() >= 500);
}

#[test]
fn indep_chunk_region_mutations_never_decode_to_different_data() {
    let f = build("indep-chunk", RunShape::new(4, 30), 4 * 1024 * 1024);
    assert_eq!(f.chunks.len(), 1, "wanted the single-chunk shape");
    let (off, len) = f.chunks[0];
    let (off, len) = (off as usize, len as usize);
    println!("INDEP-FUZZ chunk at {off}, {len} bytes, file {}", f.bytes.len());
    let mut t = Tally::new();
    // Every byte of the chunk record, three mutations each: flip bit 0, flip bit 7,
    // and substitute 0xFF. Every eighth byte additionally gets a 0x00.
    for i in off..off + len {
        for (k, m) in [("^01", 0u8), ("^80", 1), ("=FF", 2)] {
            let mut b = f.bytes.clone();
            match m {
                0 => b[i] ^= 0x01,
                1 => b[i] ^= 0x80,
                _ => b[i] = 0xFF,
            }
            if b[i] == f.bytes[i] {
                continue;
            }
            let label = format!("chunk{k}@{i}");
            let o = run_case(&label, &b, &f.scratch);
            if let Some(d) = t.add(&label, o) {
                assert_eq!(
                    d, f.clean,
                    "{label}: a corrupt chunk decoded to DIFFERENT data"
                );
            }
        }
    }
    t.report("chunk-region");
    assert!(
        t.named + t.clean >= 3 * len - 200,
        "the sweep did not cover the chunk"
    );
}

#[test]
fn indep_structured_attacks_on_the_container_layout() {
    let f = build("indep-struct", RunShape::new(6, 60), 16 * 1024);
    let mut t = Tally::new();
    let mut named_detail: Vec<(String, String)> = Vec::new();
    let mut run = |t: &mut Tally, label: String, b: Vec<u8>, detail: &mut Vec<(String, String)>| {
        let o = run_case(&label, &b, &f.scratch);
        if let Outcome::Named(ref m) = o {
            detail.push((label.clone(), m.clone()));
        }
        t.add(&label, o)
    };

    // --- the footer's summary_start
    for v in [
        0u64,
        1,
        u64::MAX,
        u64::MAX - 1,
        f.bytes.len() as u64,
        f.bytes.len() as u64 + 1,
        (f.bytes.len() / 2) as u64,
        27,
        summary_start(&f.bytes) + 1,
        summary_start(&f.bytes) - 1,
    ] {
        let mut b = f.bytes.clone();
        let o = summary_start_offset(&b);
        put_u64(&mut b, o, v);
        run(&mut t, format!("summary_start={v}"), b, &mut named_detail);
    }

    // --- the chunk record's own header fields
    for (ci, (coff, _clen)) in f.chunks.iter().enumerate() {
        let body = chunk_body(&f.bytes, *coff);
        let csz_off = chunk_compressed_size_offset(&f.bytes, *coff);
        let real_u = u64_at(&f.bytes, body + CHUNK_UNCOMPRESSED_SIZE);
        let real_c = u64_at(&f.bytes, csz_off);
        for v in [
            0u64,
            1,
            2,
            u64::MAX,
            u64::MAX / 2,
            1 << 62,
            1 << 40,
            255 * 1024 * 1024,
            257 * 1024 * 1024,
            real_u + 1,
            real_u - 1,
        ] {
            let mut b = f.bytes.clone();
            put_u64(&mut b, body + CHUNK_UNCOMPRESSED_SIZE, v);
            let label = format!("chunk{ci}.uncompressed_size={v}");
            let o = run_case(&label, &b, &f.scratch);
            if let Outcome::Clean(_) = o {
                panic!("{label}: accepted a chunk header that disagrees with the index");
            }
            if let Outcome::Named(ref m) = o {
                named_detail.push((label.clone(), m.clone()));
            }
            t.add(&label, o);
        }
        for v in [
            0u64,
            1,
            u64::MAX,
            1 << 40,
            real_c + 1,
            real_c - 1,
            real_c + 1024,
            real_c.saturating_sub(1024),
        ] {
            let mut b = f.bytes.clone();
            put_u64(&mut b, csz_off, v);
            let label = format!("chunk{ci}.compressed_size={v}");
            let o = run_case(&label, &b, &f.scratch);
            if let Outcome::Clean(_) = o {
                panic!("{label}: accepted a chunk whose compressed_size does not add up");
            }
            if let Outcome::Named(ref m) = o {
                named_detail.push((label.clone(), m.clone()));
            }
            t.add(&label, o);
        }
        // compression string length
        for v in [0u32, 1, 3, u32::MAX, 1 << 30] {
            let mut b = f.bytes.clone();
            put_u32(&mut b, body + CHUNK_COMPRESSION_LEN, v);
            run(
                &mut t,
                format!("chunk{ci}.compression_len={v}"),
                b,
                &mut named_detail,
            );
        }
    }

    // --- THE COORDINATED ATTACK: chunk header AND summary chunk index agree on a lie.
    let cidx = chunk_index_records(&f.bytes);
    assert_eq!(
        cidx.len(),
        f.chunks.len(),
        "one chunk-index record per chunk"
    );
    let mut coordinated = 0usize;
    for (ci, (istart, ilen)) in cidx.iter().enumerate() {
        // `uncompressed_size` is the last u64 of a chunk-index record body.
        let iu = istart + ilen - 8;
        let real = u64_at(&f.bytes, iu);
        let coff = f.chunks[ci].0;
        let body = chunk_body(&f.bytes, coff);
        assert_eq!(
            real,
            u64_at(&f.bytes, body + CHUNK_UNCOMPRESSED_SIZE),
            "the index copy must match the header before we lie about it"
        );
        for v in [
            0u64,
            1,
            2,
            64,
            1 << 20,
            200 * 1024 * 1024,
            255 * 1024 * 1024,
            256 * 1024 * 1024,
            257 * 1024 * 1024,
            1 << 40,
            1 << 62,
            u64::MAX,
        ] {
            let mut b = f.bytes.clone();
            put_u64(&mut b, iu, v);
            put_u64(&mut b, body + CHUNK_UNCOMPRESSED_SIZE, v);
            let label = format!("coordinated.chunk{ci}.uncompressed_size={v}");
            let o = run_case(&label, &b, &f.scratch);
            if let Outcome::Clean(_) = o {
                panic!("{label}: a coordinated lie about uncompressed_size was accepted");
            }
            if let Outcome::Named(ref m) = o {
                named_detail.push((label.clone(), m.clone()));
            }
            t.add(&label, o);
            coordinated += 1;
        }
        // The index's chunk_start_offset (body[16..24]) and chunk_length (body[24..32]).
        for (name, fo) in [("chunk_start_offset", 16usize), ("chunk_length", 24)] {
            let real = u64_at(&f.bytes, istart + fo);
            for v in [0u64, 1, u64::MAX, 1 << 40, real + 1, real - 1, real + 4096] {
                let mut b = f.bytes.clone();
                put_u64(&mut b, istart + fo, v);
                run(
                    &mut t,
                    format!("index.chunk{ci}.{name}={v}"),
                    b,
                    &mut named_detail,
                );
            }
        }
    }
    println!("INDEP-FUZZ coordinated cases: {coordinated}");

    // --- every summary record's declared length
    for (op, s, l) in summary_records(&f.bytes) {
        for v in [0u64, 1, u64::MAX, 1 << 40, l as u64 + 1, (l as u64) - 1] {
            let mut b = f.bytes.clone();
            put_u64(&mut b, s - 8, v);
            run(
                &mut t,
                format!("summary.op{op:#04x}@{s}.len={v}"),
                b,
                &mut named_detail,
            );
        }
    }

    // --- every record length in the DATA section
    let mut p = 8usize; // past the magic
    let data_end = summary_start(&f.bytes) as usize;
    let mut data_recs = 0usize;
    while p + 9 <= data_end {
        let op = f.bytes[p];
        let len = u64_at(&f.bytes, p + 1) as usize;
        if len == 0 || p + 9 + len > data_end {
            break;
        }
        for v in [0u64, 1, u64::MAX, 1 << 40, len as u64 + 1, (len as u64) - 1] {
            let mut b = f.bytes.clone();
            put_u64(&mut b, p + 1, v);
            run(
                &mut t,
                format!("data.op{op:#04x}@{p}.len={v}"),
                b,
                &mut named_detail,
            );
        }
        // and the opcode itself
        for v in [0x00u8, 0x05, 0x06, 0x0F, 0xFF] {
            if v == op {
                continue;
            }
            let mut b = f.bytes.clone();
            b[p] = v;
            run(
                &mut t,
                format!("data@{p}.opcode={v:#04x}"),
                b,
                &mut named_detail,
            );
        }
        p += 9 + len;
        data_recs += 1;
    }
    println!("INDEP-FUZZ data-section records walked: {data_recs}");

    t.report("structured");
    // A sample of the messages, so the errors are visibly named rather than empty.
    for (l, m) in named_detail.iter().take(8) {
        println!("INDEP-FUZZ  e.g. {l} -> {m}");
    }
}

#[test]
fn indep_random_smashes_and_runs_of_bits() {
    let f = build("indep-random", RunShape::new(6, 60), 16 * 1024);
    let n = f.bytes.len();
    let mut t = Tally::new();

    // Runs of inverted bytes, of several widths, striding the whole file.
    let mut runs = 0usize;
    for width in [2usize, 4, 8, 16, 64, 256] {
        let mut i = 0usize;
        while i + width <= n {
            let mut b = f.bytes.clone();
            for j in i..i + width {
                b[j] = !b[j];
            }
            let label = format!("run{width}@{i}");
            t.add(&label, run_case(&label, &b, &f.scratch));
            runs += 1;
            i += 1 + n / 64;
        }
    }
    println!("INDEP-FUZZ inverted runs: {runs}");

    // Pseudo-random multi-byte smashes across the whole file.
    let mut rng = Lcg(0xA5A5_1234_DEAD_BEEF);
    let mut smashes = 0usize;
    for _ in 0..1500 {
        let mut b = f.bytes.clone();
        let k = 1 + rng.below(6);
        for _ in 0..k {
            let at = rng.below(n);
            b[at] = (rng.next() & 0xFF) as u8;
        }
        let label = format!("smash#{smashes}");
        t.add(&label, run_case(&label, &b, &f.scratch));
        smashes += 1;
    }
    println!("INDEP-FUZZ random smashes: {smashes}");
    t.report("random");
}

#[test]
fn indep_a_recording_with_no_records_still_survives_corruption() {
    // A frames-only recording: a different message mix, so a different chunk layout.
    let dir = scratch_dir("indep-framesonly").expect("scratch");
    let path = dir.join("run.mcap");
    write_recording_with(
        &path,
        &RunShape::new(3, 24),
        RecordingOptions {
            chunk_target_bytes: 8 * 1024,
            ..Default::default()
        },
        false,
    )
    .expect("written");
    let bytes = std::fs::read(&path).expect("read");
    let scratch = dir.join("mutant.mcap");
    let mut t = Tally::new();
    let mut i = 0usize;
    let mut cases = 0usize;
    while i < bytes.len() {
        let mut b = bytes.clone();
        b[i] ^= 0xFF;
        let label = format!("framesonly@{i}");
        t.add(&label, run_case(&label, &b, &scratch));
        cases += 1;
        i += 3;
    }
    println!("INDEP-FUZZ frames-only sweep: {cases}");
    t.report("frames-only");
}

#[test]
fn indep_the_clean_fixture_is_stable() {
    // A control: the same bytes read twice give the same digest, so a "clean read"
    // above really means "read to the same content".
    let f = build("indep-control", RunShape::new(4, 30), 4 * 1024 * 1024);
    let again = read_every_path_from_bytes(f.bytes.clone()).expect("reads");
    assert_eq!(again, f.clean);
    let _ = write_recording(&f.scratch, &RunShape::new(2, 4)).expect("a second recording");
    println!("INDEP-FUZZ control ok, clean digest {}", hex(&f.clean));
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// MCAP treats `uncompressed_crc == 0` as "CRC not computed"
/// (mcap-0.25.0/src/sans_io/linear_reader.rs:622: `prevalidate_chunk_crcs && state.crc != 0`).
/// Zeroing those four bytes therefore switches the chunk's integrity check OFF, and the
/// corrupt payload behind it is decompressed and handed to the caller as content.
///
/// This test MEASURES that rather than asserting it away: it reports how many corrupt
/// chunks came back as content that differs from what was recorded, and how many of those
/// also passed `verify()`.
#[test]
fn indep_zeroing_the_chunk_crc_turns_the_integrity_check_off() {
    let f = build("indep-crc0", RunShape::new(4, 30), 4 * 1024 * 1024);
    let (coff, clen) = f.chunks[0];
    let body = chunk_body(&f.bytes, coff);
    let crc_before = u32::from_le_bytes(
        f.bytes[body + CHUNK_CRC..body + CHUNK_CRC + 4]
            .try_into()
            .unwrap(),
    );
    assert_ne!(crc_before, 0, "this crate's writer does compute a chunk CRC");
    let payload = chunk_payload_offset(&f.bytes, coff);
    let payload_end = (coff + clen) as usize;

    // Control: zeroing the CRC on an UNCORRUPTED chunk.
    let mut b = f.bytes.clone();
    put_u32(&mut b, body + CHUNK_CRC, 0);
    let control = run_case("crc0-only", &b, &f.scratch);
    println!(
        "INDEP-FUZZ crc0 control (no payload damage): {}",
        match &control {
            Outcome::Clean(d) if *d == f.clean => "read cleanly, same content".to_string(),
            Outcome::Clean(_) => "read cleanly, DIFFERENT content".to_string(),
            Outcome::Named(m) => format!("named error: {m}"),
            Outcome::Panicked(m) => format!("PANIC: {m}"),
        }
    );

    let mut errored = 0usize;
    let mut same = 0usize;
    let mut different = 0usize;
    let mut panicked = 0usize;
    let mut first_example = String::new();
    for i in (payload..payload_end).step_by(7) {
        for bit in [0u8, 3, 7] {
            let mut b = f.bytes.clone();
            put_u32(&mut b, body + CHUNK_CRC, 0);
            b[i] ^= 1 << bit;
            let label = format!("crc0+payload^{bit}@{i}");
            match run_case(&label, &b, &f.scratch) {
                Outcome::Named(_) => errored += 1,
                Outcome::Clean(d) if d == f.clean => same += 1,
                Outcome::Clean(_) => {
                    if first_example.is_empty() {
                        first_example = label.clone();
                    }
                    different += 1;
                }
                Outcome::Panicked(_) => panicked += 1,
            }
        }
    }
    println!(
        "INDEP-FUZZ crc0 + one payload bit: {} cases, {errored} named errors, \
         {same} clean-and-identical, {different} CLEAN BUT DIFFERENT CONTENT, {panicked} panics \
         (first: {first_example})",
        errored + same + different + panicked
    );
    // The one thing that must still hold whatever the CRC says: no abort, no panic.
    assert_eq!(panicked, 0, "a CRC-disabled corrupt chunk panicked");
}
