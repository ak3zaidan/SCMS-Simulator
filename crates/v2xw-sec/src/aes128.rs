//! The bare AES-128 block permutation.
//!
//! Both CAMP SCMS constructions in this crate are defined over the block cipher itself
//! rather than over a mode of operation, which is unusual enough to be worth naming:
//!
//! * butterfly-key expansion (SCP1) evaluates `AES(k, x+m) XOR (x+m)` for `m = 1, 2, 3`
//!   and concatenates the three 128-bit results into the 384-bit value it reduces modulo
//!   the group order ([`crate::butterfly`]);
//! * a pre-linkage value (SCP2) is one Davies-Meyer compression `AES(ls, b) XOR b`,
//!   truncated to 72 bits ([`crate::linkage`]).
//!
//! Both are single blocks with no chaining, no padding and no IV, so there is no mode to
//! choose and the legacy Python's `modes.ECB()` is not a mode decision either — it is how
//! you ask a library for the raw permutation. Calling it ECB *encryption* would be a
//! confidentiality claim; it is not one, and neither construction encrypts anything.
//!
//! AES is a fixed permutation, so the `aes` crate's hardware-accelerated and software
//! paths produce identical output and the choice between them cannot affect determinism
//! (ADR 0004).

use aes::Aes128;
use aes::cipher::{Block, BlockEncrypt, KeyInit};

/// One AES-128 block, in bytes.
pub const BLOCK: usize = 16;

/// The AES-128 key size, in bytes.
pub const KEY: usize = 16;

/// `AES-128(key, block)`: the raw forward permutation on one block.
pub fn encrypt_block(key: &[u8; KEY], block: &[u8; BLOCK]) -> [u8; BLOCK] {
    let cipher = Aes128::new(key.into());
    let mut b = Block::<Aes128>::from(*block);
    cipher.encrypt_block(&mut b);
    b.into()
}

/// `AES-128(key, block) XOR block`: the Davies-Meyer compression of one block.
///
/// The feed-forward XOR is what turns a permutation into a one-way compression function:
/// without it, knowing the key would let anyone invert the output, and a pre-linkage
/// value would reveal its input block.
pub fn davies_meyer(key: &[u8; KEY], block: &[u8; BLOCK]) -> [u8; BLOCK] {
    let mut out = encrypt_block(key, block);
    for (o, b) in out.iter_mut().zip(block.iter()) {
        *o ^= *b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::hash::hex_encode;

    /// FIPS-197 Appendix C.1, the published AES-128 known-answer vector. If this fails,
    /// every butterfly and linkage value in the crate is wrong, so it is checked here
    /// rather than inferred from those higher-level tests passing.
    #[test]
    fn the_fips_197_known_answer_vector_reproduces() {
        let key: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let pt: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        assert_eq!(
            hex_encode(&encrypt_block(&key, &pt)),
            "69c4e0d86a7b0430d8cdb78070b4c55a"
        );
    }

    /// Davies-Meyer is the cipher output XOR the input, and the feed-forward really is
    /// applied — a missing XOR would leave the raw ciphertext.
    #[test]
    fn davies_meyer_feeds_the_block_forward() {
        let key = [0x11u8; 16];
        let block = [0x22u8; 16];
        let raw = encrypt_block(&key, &block);
        let dm = davies_meyer(&key, &block);
        assert_ne!(dm, raw, "the feed-forward XOR must change the output");
        for i in 0..16 {
            assert_eq!(dm[i], raw[i] ^ block[i]);
        }
    }
}
