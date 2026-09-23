//! A small, exact unaligned PER engine — only the constructs the SAE J2735 messages this
//! crate hand-encodes actually need.
//!
//! Build decision D2 makes the J2735 codecs hand-written, which means hand-writing the
//! encoding rules too. This module is that: a bit writer, a bit reader, and one function
//! per ASN.1 construct the Basic Safety Message ([`crate::j2735::bsm`]), the SPaT
//! ([`crate::j2735::spat`]) and the MAP ([`crate::j2735::map`]) actually contain. It is
//! deliberately not a general PER implementation — a general one is a year of work and
//! `rasn` already is one for the types it can generate.
//!
//! # What is implemented, and the clause it comes from
//!
//! Clause numbers are ITU-T X.691 (02/2021), the UNALIGNED variant throughout.
//!
//! | Construct | Clause | Function |
//! |---|---|---|
//! | Constrained whole number, the general case | 11.5.2 | [`write_constrained_int`], [`read_constrained_int`] |
//! | Constrained length determinant | 11.9.4.1 | [`write_constrained_length`], [`read_constrained_length`] |
//! | Unconstrained length determinant, one- and two-octet forms | 11.9.3.6–11.9.3.8 | [`write_length`], [`read_length`] |
//! | `BOOLEAN` | 12 | [`write_bool`], [`read_bool`] |
//! | `ENUMERATED`, non-extensible | 14.2–14.3 | [`write_enumerated`], [`read_enumerated`] |
//! | `BIT STRING`, fixed size ≤ 64 bits | 16.6 | [`write_fixed_bit_string`], [`read_fixed_bit_string`] |
//! | `BIT STRING`, extensible size whose root is a single value | 16.3 + 16.6 | [`write_extensible_bit_string`], [`read_extensible_bit_string`] |
//! | `OCTET STRING`, fixed size | 17.4 | [`write_fixed_octet_string`], [`read_fixed_octet_string`] |
//! | `SEQUENCE` preamble: extension bit and optional-field bit-map | 19.1–19.2 | [`write_preamble`], [`read_preamble`] |
//! | Open type field | 11.2 | [`write_open_type`], [`read_open_type`] |
//! | `CHOICE` index, root alternatives only | 23.5–23.7 | [`write_choice_index`], [`read_choice_index`] |
//!
//! # What is deliberately absent
//!
//! No alignment (the U in UPER: nothing is ever padded to an octet boundary except the
//! very end of the outermost encoding and the inside of an open type), no `REAL`, no
//! character strings (so every `DescriptiveName` in a SPaT or a MAP is refused rather than
//! encoded), no semi-constrained or unconstrained integers, no fragmentation of lengths at
//! or above 16 K, no `CHOICE` extension additions, and no decoding of `SEQUENCE` extension
//! additions. Every one of those either cannot occur in the subset this crate emits or
//! produces [`UperError::Unsupported`] — never a guess.
//!
//! # Why a bit writer at all
//!
//! Because UPER packs fields with no padding between them: a `Latitude` is 31 bits and the
//! `Longitude` after it starts mid-octet. Anything that thinks in bytes gets this wrong.
//! The whole reason this module is separated from the message modules is that the bit
//! plumbing is where the defects live and it is testable in isolation, against worked
//! examples from the standard.

/// The field being encoded or decoded, for error messages that name a cause.
///
/// Carried as a pair rather than a single string because [`crate::CodecError::OutOfRange`]
/// reports the simulator's field path and the ASN.1 type separately: the path says where
/// to look and the type says which constraint was violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// Dotted path of the field, e.g. `bsm.coreData.lat`.
    pub path: &'static str,
    /// The ASN.1 type whose constraint applies, e.g. `Latitude`.
    pub asn1_type: &'static str,
}

impl Field {
    /// A field descriptor.
    pub const fn new(path: &'static str, asn1_type: &'static str) -> Self {
        Self { path, asn1_type }
    }
}

impl core::fmt::Display for Field {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({})", self.path, self.asn1_type)
    }
}

/// Anything the bit-level engine can refuse.
///
/// Separate from [`crate::CodecError`] on purpose: this type knows about bits and ASN.1
/// constraints but nothing about message types, so it can be unit-tested against X.691
/// without a `MsgType` in sight. [`crate::j2735::bsm`] maps it to a `CodecError`, which is
/// where the message type is known.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UperError {
    /// A value did not fit the constraint of its ASN.1 type.
    #[error(
        "{field}: {value} is outside the range {min}..={max} that ASN.1 type `{asn1_type}` allows"
    )]
    OutOfRange {
        /// Dotted path of the field.
        field: &'static str,
        /// The ASN.1 type whose constraint was violated.
        asn1_type: &'static str,
        /// The offending value in wire units.
        value: i64,
        /// Lowest admissible value.
        min: i64,
        /// Highest admissible value.
        max: i64,
    },

    /// The encoding ended in the middle of a field.
    #[error("truncated at bit {at}: {want} more bits were needed but only {have} remain")]
    Truncated {
        /// Bit offset the read started at.
        at: usize,
        /// Bits the field needed.
        want: usize,
        /// Bits actually left.
        have: usize,
    },

    /// An `ENUMERATED` selected an index its root list does not have.
    #[error("`{asn1_type}` has {count} root values but the encoding selected index {index}")]
    BadEnumIndex {
        /// The ASN.1 type.
        asn1_type: &'static str,
        /// The index that was encoded.
        index: u64,
        /// How many values the root list holds.
        count: u64,
    },

    /// A construct this engine does not implement, met while encoding or decoding.
    ///
    /// The whole point of the variant: an unimplemented construct must stop the codec, not
    /// be skipped. Skipping bits in a PER encoding desynchronises everything after them, so
    /// a decoder that guessed would return a plausible message built from the wrong bits.
    #[error("not implemented: {construct} — {detail}")]
    Unsupported {
        /// Where it was met, e.g. `PathHistory.initialPosition`.
        construct: &'static str,
        /// Why it is not implemented, and what a caller can do instead.
        detail: &'static str,
    },

    /// Bits were left over after the outermost value was decoded.
    ///
    /// X.691 clause 11.1 pads the outermost encoding to an octet boundary with zero bits,
    /// so at most seven zero bits may follow a complete value. Anything more, or anything
    /// non-zero, means the bytes are not the encoding of this type.
    #[error("{bits} bits of data follow the decoded value; at most 7 zero padding bits may")]
    TrailingData {
        /// How many bits were left.
        bits: usize,
    },

    /// The bytes are not a legal encoding of the type at all.
    ///
    /// Deliberately distinct from [`UperError::Unsupported`]. `Unsupported` says *this
    /// engine* does not implement something X.691 allows, so the bytes may well be a
    /// correct encoding that a fuller decoder would read. `Malformed` says the encoding
    /// rules forbid what the bytes say, so no conforming encoder could have produced them
    /// and there is nothing to implement. Keeping them apart matters because the first is
    /// a gap in this crate and the second is a defect in the sender.
    #[error("malformed encoding of {construct}: {detail}")]
    Malformed {
        /// The ASN.1 element whose encoding is illegal, e.g. `partII-Value`.
        construct: &'static str,
        /// Which rule the encoding breaks, and the clause that states it.
        detail: &'static str,
    },
}

// =========================================================================================
// Bit writer
// =========================================================================================

/// Appends bits, most-significant first, with no padding between fields.
#[derive(Debug, Clone, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    /// Bits written so far. `bytes.len() * 8 - bits` is the number of unused low bits in
    /// the last octet, which are always zero.
    bits: usize,
}

impl BitWriter {
    /// An empty writer.
    pub const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bits: 0,
        }
    }

    /// An empty writer with room for `octets` octets.
    pub fn with_capacity(octets: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(octets),
            bits: 0,
        }
    }

    /// How many bits have been written.
    pub const fn bit_len(&self) -> usize {
        self.bits
    }

    /// True when nothing has been written.
    pub const fn is_empty(&self) -> bool {
        self.bits == 0
    }

    /// Appends one bit.
    pub fn write_bit(&mut self, bit: bool) {
        if self.bits % 8 == 0 {
            self.bytes.push(0);
        }
        if bit {
            let index = self.bytes.len() - 1;
            // Bit 0 of the octet is the most significant, per X.691 clause 3.7.2.
            self.bytes[index] |= 0x80 >> (self.bits % 8);
        }
        self.bits += 1;
    }

    /// Appends the low `width` bits of `value`, most-significant first.
    ///
    /// `width` above 64 is a programming error and is clamped to 64; bits of `value` above
    /// `width` are ignored. Both are unreachable from this crate, because every caller
    /// range-checks first — which is why they are not errors.
    pub fn write_bits(&mut self, value: u64, width: u32) {
        let width = width.min(64);
        for shift in (0..width).rev() {
            self.write_bit((value >> shift) & 1 == 1);
        }
    }

    /// Appends whole octets.
    pub fn write_octets(&mut self, octets: &[u8]) {
        if self.bits % 8 == 0 {
            // The common case, and worth the branch: a bulk extend instead of 8n bit
            // writes. The invariant `bytes.len() * 8 == bits` holds here, so this is just
            // an append.
            self.bytes.extend_from_slice(octets);
            self.bits += octets.len() * 8;
        } else {
            for &octet in octets {
                self.write_bits(u64::from(octet), 8);
            }
        }
    }

    /// Appends every bit another writer holds.
    pub fn write_all(&mut self, other: &BitWriter) {
        if self.bits % 8 == 0 && other.bits % 8 == 0 {
            self.bytes.extend_from_slice(&other.bytes);
            self.bits += other.bits;
            return;
        }
        for i in 0..other.bits {
            let octet = other.bytes[i / 8];
            self.write_bit(octet & (0x80 >> (i % 8)) != 0);
        }
    }

    /// The encoding, with the last octet zero-padded (X.691 clause 11.1).
    ///
    /// An empty encoding stays empty here; the one place X.691 requires a single zero
    /// octet instead is inside an open type, and [`write_open_type`] does that.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// The encoding without consuming the writer.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

// =========================================================================================
// Bit reader
// =========================================================================================

/// Reads bits, most-significant first.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    /// A reader over an encoding.
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// How many bits the encoding holds, padding included.
    pub const fn bit_len(&self) -> usize {
        self.bytes.len() * 8
    }

    /// How many bits have been consumed.
    pub const fn bits_read(&self) -> usize {
        self.pos
    }

    /// How many bits are left.
    pub const fn remaining(&self) -> usize {
        self.bit_len() - self.pos
    }

    fn need(&self, want: usize) -> Result<(), UperError> {
        if self.remaining() < want {
            return Err(UperError::Truncated {
                at: self.pos,
                want,
                have: self.remaining(),
            });
        }
        Ok(())
    }

    /// Reads one bit.
    pub fn read_bit(&mut self) -> Result<bool, UperError> {
        self.need(1)?;
        let octet = self.bytes[self.pos / 8];
        let bit = octet & (0x80 >> (self.pos % 8)) != 0;
        self.pos += 1;
        Ok(bit)
    }

    /// Reads `width` bits as a non-negative integer, most-significant first.
    ///
    /// `width` above 64 is a programming error and is clamped, exactly as in
    /// [`BitWriter::write_bits`].
    pub fn read_bits(&mut self, width: u32) -> Result<u64, UperError> {
        let width = width.min(64) as usize;
        self.need(width)?;
        let mut value = 0u64;
        for _ in 0..width {
            value = (value << 1) | u64::from(self.read_bit()?);
        }
        Ok(value)
    }

    /// Reads `n` whole octets.
    pub fn read_octets(&mut self, n: usize) -> Result<Vec<u8>, UperError> {
        self.need(n * 8)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.read_bits(8)? as u8);
        }
        Ok(out)
    }

    /// Checks that what is left is nothing but X.691 clause 11.1 padding.
    ///
    /// Called once, after the outermost value: at most seven bits, all zero. This is what
    /// turns "the fields I expected decoded" into "these bytes are the encoding of this
    /// type", and it is how a truncated or over-long payload is caught.
    pub fn finish(&self) -> Result<(), UperError> {
        let left = self.remaining();
        if left >= 8 {
            return Err(UperError::TrailingData { bits: left });
        }
        let mut probe = self.clone();
        for _ in 0..left {
            if probe.read_bit()? {
                return Err(UperError::TrailingData { bits: left });
            }
        }
        Ok(())
    }
}

// =========================================================================================
// Constrained whole numbers — X.691 clause 11.5
// =========================================================================================

/// Bits a constrained whole number occupies in the UNALIGNED variant (clause 11.5.2).
///
/// `range` is `max - min + 1`. Zero bits when the range holds a single value, which is the
/// case clause 11.5.4 calls out and which really does occur: a `BIT STRING` whose size
/// constraint is a single value carries no length determinant for exactly this reason.
///
/// Note what this function does *not* do: clause 11.5.3's aligned-variant special cases
/// (one octet, two octets, then an indefinite-length form) do not apply here, so a
/// `Longitude` is 32 bits and not 4 octets-plus-a-length. Getting this wrong is the classic
/// way of producing an encoding that looks right for small fields and diverges on large
/// ones.
pub const fn constrained_width(range: u64) -> u32 {
    if range <= 1 {
        0
    } else {
        // Bits needed to hold range - 1, which for every range is also ceil(log2(range)).
        u64::BITS - (range - 1).leading_zeros()
    }
}

/// The range of `min..=max` as a count of values, saturating for the impossible case.
const fn range_of(min: i64, max: i64) -> u64 {
    if max < min {
        0
    } else {
        // `max - min` can overflow i64 for a full-width range; the wrapping subtraction of
        // the two-s complement values is the mathematically correct difference modulo
        // 2^64, which for max >= min is the true difference.
        (max as u64).wrapping_sub(min as u64).wrapping_add(1)
    }
}

/// Encodes a constrained whole number (clause 11.5.2).
pub fn write_constrained_int(
    w: &mut BitWriter,
    field: Field,
    value: i64,
    min: i64,
    max: i64,
) -> Result<(), UperError> {
    if value < min || value > max {
        return Err(UperError::OutOfRange {
            field: field.path,
            asn1_type: field.asn1_type,
            value,
            min,
            max,
        });
    }
    let width = constrained_width(range_of(min, max));
    w.write_bits((value as u64).wrapping_sub(min as u64), width);
    Ok(())
}

/// Decodes a constrained whole number (clause 11.5.2).
///
/// A width that is not an exact power of two admits encodings above the range — five bits
/// can say 31 where the constraint stops at 22 — and those are invalid per clause 11.5.2,
/// so they are refused rather than clamped.
pub fn read_constrained_int(
    r: &mut BitReader<'_>,
    field: Field,
    min: i64,
    max: i64,
) -> Result<i64, UperError> {
    let range = range_of(min, max);
    let raw = r.read_bits(constrained_width(range))?;
    if raw >= range {
        // Reconstruct what the encoding claimed, for a message that names a number.
        let claimed = (min as u64).wrapping_add(raw) as i64;
        return Err(UperError::OutOfRange {
            field: field.path,
            asn1_type: field.asn1_type,
            value: claimed,
            min,
            max,
        });
    }
    Ok((min as u64).wrapping_add(raw) as i64)
}

// =========================================================================================
// Length determinants — X.691 clause 11.9
// =========================================================================================

/// Encodes a length constrained to `min..=max` (clause 11.9.4.1: as a constrained whole
/// number, with no octet alignment in the UNALIGNED variant).
pub fn write_constrained_length(
    w: &mut BitWriter,
    field: Field,
    n: usize,
    min: usize,
    max: usize,
) -> Result<(), UperError> {
    write_constrained_int(w, field, n as i64, min as i64, max as i64)
}

/// Decodes a length constrained to `min..=max`.
pub fn read_constrained_length(
    r: &mut BitReader<'_>,
    field: Field,
    min: usize,
    max: usize,
) -> Result<usize, UperError> {
    Ok(read_constrained_int(r, field, min as i64, max as i64)? as usize)
}

/// Largest length this engine encodes without fragmentation (clause 11.9.3.8).
pub const MAX_UNFRAGMENTED_LENGTH: usize = 16_383;

/// Encodes an unconstrained length determinant (clause 11.9.3.6–11.9.3.7).
///
/// One octet with a clear top bit below 128; otherwise two octets introduced by `10`.
/// Lengths from 16 K up are fragmented into 16 K blocks by clause 11.9.3.8 and are refused
/// here: the only unconstrained length in the BSM is an open type's, and no BSM Part II
/// container is remotely that large.
pub fn write_length(w: &mut BitWriter, construct: &'static str, n: usize) -> Result<(), UperError> {
    if n < 128 {
        w.write_bits(n as u64, 8);
        Ok(())
    } else if n <= MAX_UNFRAGMENTED_LENGTH {
        w.write_bits(0b10, 2);
        w.write_bits(n as u64, 14);
        Ok(())
    } else {
        Err(UperError::Unsupported {
            construct,
            detail: "a length of 16384 octets or more needs X.691 clause 11.9.3.8 \
                     fragmentation, which no BSM container can require",
        })
    }
}

/// Decodes an unconstrained length determinant (clause 11.9.3.6–11.9.3.8).
pub fn read_length(r: &mut BitReader<'_>, construct: &'static str) -> Result<usize, UperError> {
    let first = r.read_bits(8)?;
    if first & 0x80 == 0 {
        return Ok(first as usize);
    }
    if first & 0x40 == 0 {
        let low = r.read_bits(8)?;
        return Ok((((first & 0x3f) << 8) | low) as usize);
    }
    Err(UperError::Unsupported {
        construct,
        detail: "a fragmented length determinant (X.691 clause 11.9.3.8) was met; this \
                 engine decodes only the one- and two-octet forms",
    })
}

// =========================================================================================
// Simple types
// =========================================================================================

/// Encodes a `BOOLEAN` (clause 12): one bit.
pub fn write_bool(w: &mut BitWriter, value: bool) {
    w.write_bit(value);
}

/// Decodes a `BOOLEAN` (clause 12).
pub fn read_bool(r: &mut BitReader<'_>) -> Result<bool, UperError> {
    r.read_bit()
}

/// Encodes a non-extensible `ENUMERATED` (clauses 14.2–14.3): the *index* of the value in
/// the root list, as a constrained whole number over `0..=count-1`.
///
/// The index, not the number in the ASN.1 — they coincide in every BSM enumeration because
/// each is declared with consecutive numbers from zero, but the distinction is the whole
/// content of clause 14.2 and is why this takes an index.
pub fn write_enumerated(
    w: &mut BitWriter,
    field: Field,
    index: u64,
    count: u64,
) -> Result<(), UperError> {
    if count == 0 || index >= count {
        return Err(UperError::BadEnumIndex {
            asn1_type: field.asn1_type,
            index,
            count,
        });
    }
    w.write_bits(index, constrained_width(count));
    Ok(())
}

/// Decodes a non-extensible `ENUMERATED` (clauses 14.2–14.3).
pub fn read_enumerated(r: &mut BitReader<'_>, field: Field, count: u64) -> Result<u64, UperError> {
    let index = r.read_bits(constrained_width(count))?;
    if index >= count {
        return Err(UperError::BadEnumIndex {
            asn1_type: field.asn1_type,
            index,
            count,
        });
    }
    Ok(index)
}

/// Encodes a `BIT STRING` whose size constraint is a single value of at most 64 bits
/// (clause 16.6): the bits alone, with no length determinant and no alignment.
///
/// `bits` holds the string right-aligned in a `u64`: bit 0 of the ASN.1 string — the one
/// the named-bit list calls `(0)` — is the most significant of the `len` used bits. That is
/// the same convention the ASN.1 notation uses when it writes `'1010'B`, and the same one
/// `pycrate` reports a `BIT STRING` value in.
///
/// # Why this is fallible
///
/// A `BIT STRING (SIZE(n))` is exactly `n` bits, so a value with a bit set above `n` is
/// not a value of the type and has no encoding. [`BitWriter::write_bits`] would drop that
/// bit silently and emit a perfectly canonical message saying something the caller never
/// asked for — the one failure mode a round-trip test cannot see, because the decode
/// agrees with the truncated encode. Refusing here gives every fixed-size bit string the
/// same guarantee the constrained integers have, at the call site that knows the field's
/// name, rather than leaving each caller to remember a mask.
pub fn write_fixed_bit_string(
    w: &mut BitWriter,
    field: Field,
    bits: u64,
    len: u32,
) -> Result<(), UperError> {
    // `len == 64` admits every `u64`, and `1u64 << 64` is undefined, so the shift is only
    // taken below the width.
    if len < 64 && bits >> len != 0 {
        return Err(UperError::OutOfRange {
            field: field.path,
            asn1_type: field.asn1_type,
            value: bits as i64,
            min: 0,
            max: ((1u64 << len) - 1) as i64,
        });
    }
    w.write_bits(bits, len);
    Ok(())
}

/// Decodes a `BIT STRING` whose size constraint is a single value of at most 64 bits.
pub fn read_fixed_bit_string(r: &mut BitReader<'_>, len: u32) -> Result<u64, UperError> {
    r.read_bits(len)
}

/// Encodes a `BIT STRING` whose size constraint is extensible with a single-value root,
/// which is how `ExteriorLights` (`SIZE(9, ...)`) and `VehicleEventFlags`
/// (`SIZE(13, ..., 14)`) are declared.
///
/// Clause 16.3 puts one bit in front saying whether the length is in the extension root;
/// clause 16.6 then encodes the root case with no length determinant at all, because the
/// root permits exactly one length. Lengths outside the root are refused: encoding one
/// needs an unconstrained length determinant and the extra named bits, and the simulator
/// never sets them.
///
/// Fallible for the same reason [`write_fixed_bit_string`] is, and by delegating to it: a
/// bit above the root is a value this codec cannot encode, and dropping it would be a
/// silent mis-encoding. The two callers in [`crate::j2735::bsm`] check the root first and
/// refuse with a message naming the bit, so in this crate the error below is the second
/// line of defence rather than the first.
pub fn write_extensible_bit_string(
    w: &mut BitWriter,
    field: Field,
    bits: u64,
    root_len: u32,
) -> Result<(), UperError> {
    w.write_bit(false);
    write_fixed_bit_string(w, field, bits, root_len)
}

/// Decodes a `BIT STRING` whose size constraint is extensible with a single-value root.
pub fn read_extensible_bit_string(
    r: &mut BitReader<'_>,
    construct: &'static str,
    root_len: u32,
) -> Result<u64, UperError> {
    if r.read_bit()? {
        return Err(UperError::Unsupported {
            construct,
            detail: "the bit string's length is outside its extension root; this codec \
                     encodes and decodes only the root length",
        });
    }
    r.read_bits(root_len)
}

/// Encodes an `OCTET STRING` whose size constraint is a single value below 64 K
/// (clause 17.4): the octets alone, with no length determinant.
///
/// Note the unaligned variant's crucial difference from aligned PER: clause 17.4's
/// two-octet special case and the octet alignment that follows it apply only to the ALIGNED
/// variant, so a four-octet `TemporaryID` is 32 bits starting wherever the previous field
/// ended.
pub fn write_fixed_octet_string(w: &mut BitWriter, octets: &[u8]) {
    w.write_octets(octets);
}

/// Decodes an `OCTET STRING` whose size constraint is a single value below 64 K.
pub fn read_fixed_octet_string(r: &mut BitReader<'_>, n: usize) -> Result<Vec<u8>, UperError> {
    r.read_octets(n)
}

// =========================================================================================
// SEQUENCE preamble — X.691 clause 19
// =========================================================================================

/// What a `SEQUENCE` preamble said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preamble {
    /// Clause 19.2's bit-map, one bit per `OPTIONAL` or `DEFAULT` field, in declaration
    /// order, bit `i` in `1 << i`.
    ///
    /// There is no field for clause 19.1's extension bit because a set one never reaches a
    /// caller: [`read_preamble`] refuses it.
    pub present: u64,
}

impl Preamble {
    /// Whether optional field `index` (in declaration order) is present.
    pub const fn has(&self, index: usize) -> bool {
        self.present & (1 << index) != 0
    }
}

/// Encodes a `SEQUENCE` preamble (clauses 19.1–19.2).
///
/// `extensible` is whether the `SEQUENCE` carries `...`, and adds the one extension bit —
/// always encoded as absent, because nothing here fills an extension addition. `present`
/// is one entry per `OPTIONAL` field, in declaration order.
pub fn write_preamble(w: &mut BitWriter, extensible: bool, present: &[bool]) {
    if extensible {
        w.write_bit(false);
    }
    for &p in present {
        w.write_bit(p);
    }
}

/// Decodes a `SEQUENCE` preamble (clauses 19.1–19.2).
///
/// An extension bit that is set stops the decode. It has to: clause 19.2's extension
/// additions are a bit-map plus a sequence of open types whose count is only knowable from
/// the bit-map, and while that *is* skippable in principle, doing it half-right is how a
/// decoder silently returns a message assembled from misaligned bits. `construct` names the
/// `SEQUENCE` so the error says which one.
pub fn read_preamble(
    r: &mut BitReader<'_>,
    construct: &'static str,
    extensible: bool,
    optionals: usize,
) -> Result<Preamble, UperError> {
    if extensible && r.read_bit()? {
        return Err(UperError::Unsupported {
            construct,
            detail: "the encoder set the extension bit, so the value carries extension \
                     additions from a later edition of the standard that this codec \
                     cannot interpret",
        });
    }
    if optionals > 64 {
        // Clamping the loop to 64 was the alternative, and it is the one shape of bug this
        // module exists to make impossible: the remaining preamble bits would stay in the
        // stream and every field after them would be read one or more bits early. PER
        // carries no tags and no lengths, so nothing downstream would notice. `present` is
        // a `u64` because no `SEQUENCE` this codec decodes has more than four `OPTIONAL`
        // fields; if one ever does, this refusal is the signal to widen the bit-map.
        return Err(UperError::Unsupported {
            construct,
            detail: "a SEQUENCE with more than 64 OPTIONAL or DEFAULT fields; the \
                     clause 19.2 bit-map does not fit the engine's 64-bit `present` word, \
                     and reading part of it would desynchronise every field after it",
        });
    }
    let mut present = 0u64;
    for i in 0..optionals {
        if r.read_bit()? {
            present |= 1 << i;
        }
    }
    Ok(Preamble { present })
}

// =========================================================================================
// Open types — X.691 clause 11.2
// =========================================================================================

/// Encodes an open type field (clause 11.2): the complete encoding of the inner value,
/// padded to a whole number of octets, prefixed by an unconstrained length determinant.
///
/// The padding is the subtle part. Clause 11.2.1 says the inner value is encoded as if it
/// were the outermost one, which brings in clause 11.1's pad-to-an-octet-boundary; the
/// octets that result are then a length-prefixed octet string in the *outer* encoding,
/// written at whatever bit offset the outer encoding has reached. So the inner encoding is
/// octet-padded but the open type as a whole is not octet-aligned. An empty inner encoding
/// becomes one zero octet, per clause 11.2.2.
pub fn write_open_type(
    w: &mut BitWriter,
    construct: &'static str,
    inner: &[u8],
) -> Result<(), UperError> {
    if inner.is_empty() {
        write_length(w, construct, 1)?;
        w.write_bits(0, 8);
        return Ok(());
    }
    write_length(w, construct, inner.len())?;
    w.write_octets(inner);
    Ok(())
}

/// Decodes an open type field (clause 11.2), returning the inner octets.
///
/// A zero length determinant is [`UperError::Malformed`], not an empty value. Clause 11.2.2
/// makes an open type at least one octet long: an inner encoding with no bits is padded up
/// to a single zero octet, which is exactly what [`write_open_type`] emits. So zero is not
/// the encoding of anything, and accepting it would hand back a value that re-encodes to
/// one octet more than it decoded from — the round trip would fail at the codec seam with a
/// byte count instead of here with the reason.
pub fn read_open_type(
    r: &mut BitReader<'_>,
    construct: &'static str,
) -> Result<Vec<u8>, UperError> {
    let n = read_length(r, construct)?;
    if n == 0 {
        return Err(UperError::Malformed {
            construct,
            detail: "the open type's length determinant is zero, but X.691 clause 11.2.2 \
                     encodes an empty inner value as a single zero octet, so an open type \
                     is never shorter than one octet",
        });
    }
    r.read_octets(n)
}

// =========================================================================================
// CHOICE — X.691 clause 23
// =========================================================================================

/// Encodes a `CHOICE`'s alternative selector (clauses 23.5–23.7).
///
/// An extensible `CHOICE` gets one bit saying whether the chosen alternative is in the
/// extension root (clause 23.5); it is always written as *in the root*, because nothing
/// here selects an extension addition. The root index then follows as a constrained whole
/// number over `0..=root_count-1` (clause 23.7), which is zero bits wide when the root has
/// a single alternative — the same clause 11.5.4 degenerate case a single-value integer
/// range hits, and just as easy to get wrong by emitting a stray bit.
///
/// `index` is the alternative's position in the **canonical order** of the root
/// alternatives, which for every `CHOICE` this codec encodes is textual declaration order:
/// J2735 declares no `CHOICE` whose alternatives carry tags out of order, so the two
/// coincide. The caller supplies the index rather than a name so that this stays a pure
/// bit-level function, exactly like [`write_enumerated`].
pub fn write_choice_index(
    w: &mut BitWriter,
    field: Field,
    extensible: bool,
    index: u64,
    root_count: u64,
) -> Result<(), UperError> {
    if root_count == 0 || index >= root_count {
        // `BadEnumIndex` rather than a CHOICE-specific variant: the fault, the fields and
        // the sentence that describes it ("N root values, index i selected") are identical,
        // and one variant that is always right beats two that have to be kept in step.
        return Err(UperError::BadEnumIndex {
            asn1_type: field.asn1_type,
            index,
            count: root_count,
        });
    }
    if extensible {
        w.write_bit(false);
    }
    w.write_bits(index, constrained_width(root_count));
    Ok(())
}

/// Decodes a `CHOICE`'s alternative selector (clauses 23.5–23.7).
///
/// A set extension bit is [`UperError::Unsupported`], never a skipped value: clause 23.8
/// encodes an extension addition as a length-prefixed open type, and while that *could* be
/// stepped over, the value inside it would be lost and any later field of the enclosing
/// `SEQUENCE` would then be interpreted against a `CHOICE` this codec did not understand.
/// Refusing says so; skipping would hand back a message with a silently missing turn
/// restriction or node offset.
pub fn read_choice_index(
    r: &mut BitReader<'_>,
    field: Field,
    construct: &'static str,
    extensible: bool,
    root_count: u64,
) -> Result<u64, UperError> {
    if extensible && r.read_bit()? {
        return Err(UperError::Unsupported {
            construct,
            detail: "the CHOICE selects an extension addition from a later edition of the \
                     standard; its open type carries a value this codec cannot interpret",
        });
    }
    let index = r.read_bits(constrained_width(root_count))?;
    if index >= root_count {
        return Err(UperError::BadEnumIndex {
            asn1_type: field.asn1_type,
            index,
            count: root_count,
        });
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_of(bytes: &[u8], count: usize) -> String {
        (0..count)
            .map(|i| {
                if bytes[i / 8] & (0x80 >> (i % 8)) != 0 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect()
    }

    const F: Field = Field::new("test", "Test");

    #[test]
    fn a_constrained_width_is_the_bits_needed_for_the_range() {
        // The BSM's own widths, which is the list that matters.
        for (range, width) in [
            (1u64, 0u32),
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 3),
            (23, 5),  // PathHistoryPointList SIZE(1..23)
            (128, 7), // MsgCount 0..127
            (241, 8), // CoarseHeading 0..240
            (255, 8), // VerticalAcceleration -127..127
            (256, 8), // SemiMajorAxisAccuracy 0..255
            (257, 9),
            (4002, 12),  // Acceleration -2000..2001
            (4096, 12),  // VertOffset-B12
            (8192, 13),  // Speed 0..8191
            (28801, 15), // Heading 0..28800
            (65535, 16), // YawRate -32767..32767
            (65536, 16), // Elevation -4096..61439
            (65537, 17),
            (262144, 18),        // OffsetLL-B18
            (1_800_000_002, 31), // Latitude
            (3_600_000_001, 32), // Longitude
        ] {
            assert_eq!(
                constrained_width(range),
                width,
                "range {range} should take {width} bits"
            );
        }
    }

    /// The one place it is easy to be wrong by a whole field: X.691's UNALIGNED variant has
    /// no octet special cases, so a 32-bit range really is 32 bits and not four octets with
    /// a length.
    #[test]
    fn a_wide_constrained_integer_is_minimum_bits_not_octets() {
        let mut w = BitWriter::new();
        write_constrained_int(&mut w, F, 1, 0, 3_600_000_000).expect("in range");
        assert_eq!(w.bit_len(), 32);
    }

    #[test]
    fn a_single_value_range_takes_no_bits_at_all() {
        let mut w = BitWriter::new();
        write_constrained_int(&mut w, F, 7, 7, 7).expect("in range");
        assert_eq!(w.bit_len(), 0);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(read_constrained_int(&mut r, F, 7, 7).expect("reads"), 7);
        assert_eq!(r.bits_read(), 0);
    }

    #[test]
    fn negative_lower_bounds_are_offset_not_two_s_complement() {
        // -2000..2001 is 4002 values in 12 bits; -2000 must be all zeros, not 0xF830.
        let mut w = BitWriter::new();
        write_constrained_int(&mut w, F, -2000, -2000, 2001).expect("in range");
        write_constrained_int(&mut w, F, 0, -2000, 2001).expect("in range");
        let bytes = w.to_bytes();
        assert_eq!(&bits_of(&bytes, 24)[..12], "000000000000");
        assert_eq!(&bits_of(&bytes, 24)[12..], "011111010000"); // 2000
    }

    #[test]
    fn an_out_of_range_value_is_refused_by_name() {
        let mut w = BitWriter::new();
        let err = write_constrained_int(
            &mut w,
            Field::new("bsm.coreData.lat", "Latitude"),
            900_000_002,
            -900_000_000,
            900_000_001,
        )
        .expect_err("out of range");
        assert!(
            err.to_string().contains("bsm.coreData.lat") && err.to_string().contains("Latitude"),
            "{err}"
        );
    }

    /// Five bits can encode 31 while the constraint stops at 22. X.691 makes that invalid
    /// and a decoder that clamped would invent a plausible point.
    #[test]
    fn an_encoding_above_the_range_is_refused_rather_than_clamped() {
        let mut w = BitWriter::new();
        w.write_bits(31, 5);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let err = read_constrained_int(&mut r, F, 1, 23).expect_err("above the range");
        assert!(
            matches!(err, UperError::OutOfRange { value: 32, .. }),
            "{err}"
        );
    }

    /// A `BIT STRING (SIZE(n))` has no encoding for a bit above `n`. The writer used to
    /// drop it and emit a canonical message saying something else — the failure a
    /// round-trip test cannot see, because the decode agrees with the truncated encode.
    #[test]
    fn a_bit_above_a_fixed_bit_strings_size_is_refused_not_truncated() {
        for (bits, len) in [(0b10_0000u64, 5u32), (0xFF, 5), (0x1_00, 8), (u64::MAX, 63)] {
            let mut w = BitWriter::new();
            let err = write_fixed_bit_string(&mut w, F, bits, len)
                .expect_err("a bit above the size constraint has no encoding");
            assert!(matches!(err, UperError::OutOfRange { min: 0, .. }), "{err}");
            // Nothing was written: a refused field must not leave half a value behind.
            assert_eq!(w.bit_len(), 0);
        }
        // The boundary on both sides, and the 64-bit width where no bit can be above.
        let mut w = BitWriter::new();
        write_fixed_bit_string(&mut w, F, 0b1_1111, 5).expect("31 fits five bits");
        write_fixed_bit_string(&mut w, F, u64::MAX, 64).expect("every u64 fits 64 bits");
        assert_eq!(w.bit_len(), 69);

        // And the extensible form inherits the guarantee.
        let mut w = BitWriter::new();
        let err = write_extensible_bit_string(&mut w, F, 1 << 13, 13)
            .expect_err("a bit above the extension root has no root encoding");
        assert!(
            matches!(err, UperError::OutOfRange { max: 8191, .. }),
            "{err}"
        );
    }

    #[test]
    fn bits_pack_across_octet_boundaries_with_no_padding() {
        let mut w = BitWriter::new();
        w.write_bit(true);
        w.write_bits(0, 3);
        w.write_octets(&[0xff, 0x00]);
        w.write_bits(0b101, 3);
        assert_eq!(w.bit_len(), 1 + 3 + 16 + 3);
        let bytes = w.to_bytes();
        assert_eq!(bits_of(&bytes, 23), "10001111111100000000101");
        assert_eq!(bytes.len(), 3); // 23 bits, zero-padded to 24
        assert_eq!(bytes[2] & 0x01, 0);
    }

    #[test]
    fn everything_written_reads_back_identically() {
        let mut w = BitWriter::new();
        write_preamble(&mut w, true, &[true, false, true]);
        write_constrained_int(&mut w, F, -4096, -4096, 61439).expect("in range");
        write_bool(&mut w, true);
        write_enumerated(&mut w, F, 3, 8).expect("valid index");
        write_fixed_bit_string(&mut w, F, 0b10101, 5).expect("in range");
        write_extensible_bit_string(&mut w, F, 0b1_0000_0000, 9).expect("in range");
        write_fixed_octet_string(&mut w, &[0xde, 0xad, 0xbe, 0xef]);
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        let pre = read_preamble(&mut r, "Test", true, 3).expect("preamble");
        assert!(pre.has(0) && !pre.has(1) && pre.has(2));
        assert_eq!(
            read_constrained_int(&mut r, F, -4096, 61439).expect("int"),
            -4096
        );
        assert!(read_bool(&mut r).expect("bool"));
        assert_eq!(read_enumerated(&mut r, F, 8).expect("enum"), 3);
        assert_eq!(read_fixed_bit_string(&mut r, 5).expect("bits"), 0b10101);
        assert_eq!(
            read_extensible_bit_string(&mut r, "Test", 9).expect("bits"),
            0b1_0000_0000
        );
        assert_eq!(
            read_fixed_octet_string(&mut r, 4).expect("octets"),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        r.finish().expect("nothing but padding left");
    }

    #[test]
    fn a_set_extension_bit_stops_the_decode_instead_of_guessing() {
        let mut w = BitWriter::new();
        w.write_bit(true); // the extension bit
        w.write_bits(0, 7);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let err = read_preamble(&mut r, "BSMcoreData", true, 2).expect_err("refused");
        assert!(matches!(
            err,
            UperError::Unsupported {
                construct: "BSMcoreData",
                ..
            }
        ));
    }

    #[test]
    fn an_open_type_pads_its_inner_encoding_to_octets_but_is_not_itself_aligned() {
        let mut inner = BitWriter::new();
        inner.write_bits(0b101, 3); // 3 bits -> one octet, 0b10100000
        let inner = inner.into_bytes();
        assert_eq!(inner, vec![0b1010_0000]);

        let mut w = BitWriter::new();
        w.write_bits(0b1, 1); // put the open type at a non-octet offset
        write_open_type(&mut w, "Test", &inner).expect("writes");
        assert_eq!(w.bit_len(), 1 + 8 + 8, "length determinant plus one octet");
        let bytes = w.to_bytes();
        // one offset bit, the length determinant 0x01, then the padded inner octet.
        assert_eq!(bits_of(&bytes, 17), "10000000110100000");
    }

    /// X.691 clause 11.2.2 gives an empty inner value a single zero octet, so a length
    /// determinant of zero is not the encoding of anything — and [`write_open_type`] has
    /// always emitted the zero octet. A reader that accepted zero handed back a value that
    /// re-encodes one octet longer than it decoded from, so the failure surfaced as a byte
    /// count at the codec seam instead of as the malformed determinant it is.
    #[test]
    fn a_zero_length_open_type_is_refused_rather_than_read_as_empty() {
        // The one-octet form of the length determinant, value 0 (clause 11.9.3.6).
        let mut w = BitWriter::new();
        write_length(&mut w, "partII-Value", 0).expect("0 is an encodable length");
        assert_eq!(w.bit_len(), 8);
        let bytes = w.into_bytes();
        assert_eq!(bytes, vec![0x00]);

        let mut r = BitReader::new(&bytes);
        let err = read_open_type(&mut r, "partII-Value").expect_err("clause 11.2.2 forbids it");
        assert!(
            matches!(
                err,
                UperError::Malformed {
                    construct: "partII-Value",
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("11.2.2"), "{err}");

        // The shortest legal open type is one octet, and it still round-trips to nothing.
        let mut w = BitWriter::new();
        write_open_type(&mut w, "partII-Value", &[]).expect("writes the clause 11.2.2 form");
        let bytes = w.into_bytes();
        assert_eq!(bytes, vec![0x01, 0x00], "length 1, then the zero octet");
        let mut r = BitReader::new(&bytes);
        assert_eq!(
            read_open_type(&mut r, "partII-Value").expect("one octet is legal"),
            vec![0x00]
        );
    }

    /// The bit-map is a `u64`, so a `SEQUENCE` with more than 64 `OPTIONAL` fields cannot be
    /// represented. Reading the first 64 bits and stopping would leave the rest of the
    /// preamble in the stream and shift every field after it — a silent wrong-bytes path,
    /// where everything else in this module refuses by name.
    #[test]
    fn a_preamble_wider_than_the_bit_map_is_refused_rather_than_truncated() {
        // 65 optional-field bits, all set, then one field: a single `true` bit.
        let mut w = BitWriter::new();
        for _ in 0..65 {
            w.write_bit(true);
        }
        w.write_bit(true);
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        let err = read_preamble(&mut r, "WideSequence", false, 65).expect_err("does not fit");
        assert!(
            matches!(
                err,
                UperError::Unsupported {
                    construct: "WideSequence",
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("64 OPTIONAL"), "{err}");
        // Nothing was consumed, so the refusal cannot be mistaken for a partial read.
        assert_eq!(r.bits_read(), 0);

        // 64 is the boundary and still works, bit 63 included.
        let mut r = BitReader::new(&bytes);
        let pre = read_preamble(&mut r, "WideSequence", false, 64).expect("64 fits");
        assert_eq!(pre.present, u64::MAX);
        assert!(pre.has(63));
        assert_eq!(r.bits_read(), 64);
    }

    #[test]
    fn length_determinants_take_the_short_form_below_128() {
        for (n, bits) in [(0usize, 8u32), (1, 8), (127, 8), (128, 16), (16_383, 16)] {
            let mut w = BitWriter::new();
            write_length(&mut w, "Test", n).expect("encodable");
            assert_eq!(w.bit_len(), bits as usize, "length {n}");
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            assert_eq!(read_length(&mut r, "Test").expect("reads"), n);
        }
        let mut w = BitWriter::new();
        assert!(matches!(
            write_length(&mut w, "Test", 16_384),
            Err(UperError::Unsupported { .. })
        ));
    }

    #[test]
    fn a_truncated_encoding_says_where_it_ran_out() {
        let bytes = [0xffu8];
        let mut r = BitReader::new(&bytes);
        let err = r.read_bits(9).expect_err("truncated");
        assert!(
            matches!(
                err,
                UperError::Truncated {
                    at: 0,
                    want: 9,
                    have: 8
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn trailing_data_beyond_padding_is_refused() {
        let bytes = [0x00u8, 0x00];
        let mut r = BitReader::new(&bytes);
        r.read_bits(8).expect("reads");
        r.finish().expect_err("a whole spare octet is not padding");

        let bytes = [0x01u8];
        let mut r = BitReader::new(&bytes);
        r.read_bits(7).expect("reads");
        r.finish().expect_err("non-zero padding");
    }

    #[test]
    fn write_all_splices_bits_at_any_offset() {
        let mut a = BitWriter::new();
        a.write_bits(0b101, 3);
        let mut b = BitWriter::new();
        b.write_bits(0b110011, 6);
        a.write_all(&b);
        assert_eq!(a.bit_len(), 9);
        assert_eq!(bits_of(&a.to_bytes(), 9), "101110011");
    }

    /// Clause 23.7's index is as wide as the *root* alternative count, and clause 23.5's
    /// extension bit is one more bit in front of it. Both widths are checked here against
    /// the two shapes J2735 actually uses: `NodeOffsetPointXY` (8 root alternatives, no
    /// extension marker) and `LaneTypeAttributes` (8 root alternatives, extensible).
    #[test]
    fn a_choice_index_is_three_bits_for_eight_alternatives_plus_the_extension_bit() {
        let field = Field::new("test.choice", "TestChoice");

        let mut w = BitWriter::new();
        write_choice_index(&mut w, field, false, 5, 8).expect("in the root");
        assert_eq!(w.bit_len(), 3);
        assert_eq!(bits_of(&w.to_bytes(), 3), "101");

        let mut w = BitWriter::new();
        write_choice_index(&mut w, field, true, 5, 8).expect("in the root");
        assert_eq!(w.bit_len(), 4);
        assert_eq!(bits_of(&w.to_bytes(), 4), "0101");

        // A single-alternative root carries no index at all (clause 11.5.4), and an
        // extensible one then costs exactly its extension bit.
        let mut w = BitWriter::new();
        write_choice_index(&mut w, field, false, 0, 1).expect("the only alternative");
        assert_eq!(w.bit_len(), 0);

        for (extensible, root, index) in [(false, 8u64, 3u64), (true, 2, 1), (true, 8, 7)] {
            let mut w = BitWriter::new();
            write_choice_index(&mut w, field, extensible, index, root).expect("encodes");
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            assert_eq!(
                read_choice_index(&mut r, field, "TestChoice", extensible, root).expect("reads"),
                index
            );
        }
    }

    #[test]
    fn a_choice_extension_addition_is_refused_rather_than_skipped() {
        let field = Field::new("test.choice", "TestChoice");
        let mut w = BitWriter::new();
        // An encoder from a later edition: extension bit set, then an open type.
        w.write_bit(true);
        write_open_type(&mut w, "TestChoice", &[0x2a]).expect("encodes");
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let err = read_choice_index(&mut r, field, "TestChoice", true, 8)
            .expect_err("an extension addition cannot be interpreted");
        assert!(matches!(err, UperError::Unsupported { .. }), "{err}");

        // And an index the root does not have is refused by name rather than clamped.
        let mut w = BitWriter::new();
        let err = write_choice_index(&mut w, field, false, 8, 8).expect_err("no such index");
        assert!(matches!(err, UperError::BadEnumIndex { count: 8, .. }), "{err}");
    }
}
