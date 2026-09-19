//! SHA-256 helpers shared by the RNG key derivation, the content-addressed stores and
//! the run manifest, plus the canonical JSON encoding everything content-addressed is
//! hashed from.
//!
//! Hex output is always lower-case, because the legacy engine's `hashlib.hexdigest()`
//! is lower-case and the aggregate data digest hashes the hex text itself
//! (02-architecture.md §6.5, [`crate::manifest::Manifest::finalize`]).

use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::Result;

/// SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// Lower-case hex encoding of `bytes`.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // `write!` cannot fail on a String; the manual table keeps this allocation-free
        // per byte and independent of the `std::fmt` machinery.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Lower-case hex SHA-256 of `bytes`, the form written into the manifest.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&sha256(bytes))
}

/// An incremental SHA-256 that is also a [`std::io::Write`] sink.
///
/// [`sha256`] needs the whole artefact in memory at once. A recording is not: the MCAP
/// stream, a Parquet shard or an exported dataset is written incrementally and may be
/// gigabytes, and the manifest still needs its SHA-256 for
/// [`crate::manifest::Manifest::add_file_digest`] and the aggregate data digest
/// (02-architecture.md §6.5). This type lets a writer digest the bytes as they go past:
///
/// ```
/// use std::io::Write;
/// use v2xw_core::hash::{Sha256Writer, sha256_hex};
///
/// let mut digest = Sha256Writer::new();
/// for chunk in [b"first ".as_slice(), b"second".as_slice()] {
///     digest.write_all(chunk).unwrap();       // …and write the same chunk to the file
/// }
/// assert_eq!(digest.bytes_written(), 12);
/// assert_eq!(digest.finish_hex(), sha256_hex(b"first second"));
/// ```
///
/// Because it implements [`std::io::Write`], it also composes: `std::io::copy` into it,
/// `serde_json::to_writer` into it, or a tee that forwards each chunk to both the file and
/// one of these. The digest of a stream is identical to the digest of the same bytes hashed
/// in one call — chunk boundaries are invisible, which is the property a resumed or
/// buffered writer needs.
///
/// Writing never fails: every [`std::io::Write`] method returns `Ok`, and `flush` is a
/// no-op, because there is nothing behind it to fail.
#[derive(Clone)]
pub struct Sha256Writer {
    hasher: Sha256,
    bytes: u64,
}

impl Sha256Writer {
    /// A fresh hasher over zero bytes.
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    /// Absorbs `bytes`. The inherent spelling of [`std::io::Write::write_all`], for callers
    /// that do not want the `io` import or the `Result`.
    pub fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.bytes += bytes.len() as u64;
    }

    /// How many bytes have been absorbed so far.
    ///
    /// Recorders report this as the file's size, so the manifest's size and digest describe
    /// the same byte stream by construction.
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Consumes the writer and returns the digest.
    pub fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }

    /// Consumes the writer and returns the digest as lower-case hex — the form
    /// [`crate::manifest::Manifest`] carries.
    pub fn finish_hex(self) -> String {
        hex_encode(&self.finish())
    }
}

impl Default for Sha256Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Sha256Writer {
    /// Shows the byte count only: a partial SHA-256 state has no useful rendering, and
    /// printing one would invite treating it as an identity.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sha256Writer")
            .field("bytes_written", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl std::io::Write for Sha256Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.update(buf);
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.update(buf);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Canonical JSON bytes of `value`: compact, with every object's keys in sorted order at
/// every level of nesting.
///
/// This is what every content hash in the engine is computed over — model cards
/// ([`crate::card::ModelCard::canonical_bytes`]), parameter sets
/// ([`crate::registry::ParamSet::canonical_bytes`]), the scenario hash — so two runs that
/// built the same object by different routes (YAML authoring, JSON round trip, programmatic
/// construction) hash identically.
///
/// # Why the keys are sorted explicitly
///
/// A `serde_json::Value`'s object map is a `BTreeMap` — *unless* the `preserve_order`
/// feature is enabled, when it becomes an `IndexMap` that keeps insertion order. Cargo
/// unifies features across the whole dependency graph, so any future dependency of any
/// crate in the workspace (an exporter, a plug-in, even a dev-dependency) turning that
/// feature on would silently reorder the canonical bytes of every model card, and with
/// them every registry content hash, every `PluginPin` and every manifest that pins one.
/// Sorting here removes the dependency on that feature: keys are inserted in sorted order,
/// which is what both map implementations then emit.
pub fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>> {
    let v = sort_keys(serde_json::to_value(value)?);
    Ok(serde_json::to_vec(&v)?)
}

/// Rebuilds a JSON value with every object's keys in sorted order, recursively.
fn sort_keys(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> =
                map.into_iter().map(|(k, v)| (k, sort_keys(v))).collect();
            let mut out = serde_json::Map::with_capacity(sorted.len());
            for (k, v) in sorted {
                out.insert(k, v);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_keys).collect())
        }
        scalar => scalar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // Standard test vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hex_is_lower_case_and_padded() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xff]), "000fff");
    }

    /// Keys are sorted at every level, whatever order the struct declares them in and
    /// whatever `serde_json::Value`'s map type happens to be.
    #[test]
    fn canonical_json_sorts_every_level() {
        #[derive(serde::Serialize)]
        struct Inner {
            zulu: u8,
            alpha: u8,
        }
        #[derive(serde::Serialize)]
        struct Outer {
            z: Inner,
            a: Vec<Inner>,
            m: bool,
        }
        let v = Outer {
            z: Inner { zulu: 1, alpha: 2 },
            a: vec![Inner { zulu: 3, alpha: 4 }],
            m: true,
        };
        assert_eq!(
            String::from_utf8(canonical_json(&v).unwrap()).unwrap(),
            r#"{"a":[{"alpha":4,"zulu":3}],"m":true,"z":{"alpha":2,"zulu":1}}"#
        );
        // Declaration order is what plain serialisation gives, which is why it is not used.
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"z":{"zulu":1,"alpha":2},"a":[{"zulu":3,"alpha":4}],"m":true}"#
        );
    }

    /// The property a streaming recorder depends on: how the bytes were chopped up cannot
    /// show in the digest, and an empty stream is the empty digest.
    #[test]
    fn streaming_digest_equals_the_one_shot_digest() {
        use std::io::Write;

        let payload: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let expected = sha256(&payload);

        for chunk in [1usize, 7, 64, 4_096, 10_000, 65_536] {
            let mut w = Sha256Writer::new();
            for part in payload.chunks(chunk) {
                w.write_all(part).unwrap();
            }
            assert_eq!(w.bytes_written(), payload.len() as u64);
            assert_eq!(
                w.finish(),
                expected,
                "chunk size {chunk} changed the digest"
            );
        }

        // The inherent spelling and the `io::Write` spelling are the same operation.
        let mut inherent = Sha256Writer::new();
        inherent.update(&payload);
        assert_eq!(inherent.finish(), expected);

        // `std::io::copy` and the standard vectored/`write!` paths all land in `update`.
        let mut copied = Sha256Writer::new();
        std::io::copy(&mut payload.as_slice(), &mut copied).unwrap();
        assert_eq!(copied.bytes_written(), payload.len() as u64);
        assert_eq!(copied.finish_hex(), sha256_hex(&payload));

        let empty = Sha256Writer::new();
        assert_eq!(empty.bytes_written(), 0);
        assert_eq!(
            Sha256Writer::default().finish_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(empty.finish(), sha256(b""));
    }

    /// `write` reports the whole buffer as written, so `write_all` never loops, and the
    /// two halves of a split write digest as one stream.
    #[test]
    fn the_write_impl_consumes_everything_it_is_given() {
        use std::io::Write;

        let mut w = Sha256Writer::new();
        assert_eq!(w.write(b"abc").unwrap(), 3);
        assert_eq!(w.write(&[]).unwrap(), 0);
        w.flush().unwrap();
        assert_eq!(w.bytes_written(), 3);
        assert_eq!(w.finish(), sha256(b"abc"));

        let mut serialised = Sha256Writer::new();
        serde_json::to_writer(&mut serialised, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(serialised.finish_hex(), sha256_hex(br#"{"a":1}"#));

        assert!(format!("{:?}", Sha256Writer::new()).contains("bytes_written"));
    }

    #[test]
    fn canonical_json_is_independent_of_input_key_order() {
        let a: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":3}}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"a":{"x":3,"y":2},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a).unwrap(), canonical_json(&b).unwrap());
        assert_eq!(
            String::from_utf8(canonical_json(&a).unwrap()).unwrap(),
            r#"{"a":{"x":3,"y":2},"b":1}"#
        );
    }
}
