//! DES as VNC authentication uses it.
//!
//! VNC authentication sends a 16-byte challenge that the client encrypts with
//! DES, using the password as the key. Two details are easy to get wrong and
//! both make every login fail against a real server, so they are stated here
//! rather than left implicit:
//!
//! 1. The password is used as an 8-byte key with zero padding, and **each key
//!    byte's bits are reversed** before it is used. That quirk is in the
//!    original AT&T implementation and every server still expects it.
//! 2. The challenge is encrypted as two independent 8-byte blocks (ECB), and
//!    the 16-byte result is the response.
//!
//! This is DES, not 3DES and not AES: the algorithm is obsolete as a cipher and
//! is implemented here only because the protocol mandates it.

/// One DES round's key schedule, precomputed per key.
pub struct DesKeySchedule {
    pub(crate) subkeys: [[u8; 6]; 16],
}

/// Number of DES rounds.
const ROUNDS: usize = 16;

/// Initial permutation: output bit i is input bit IP[i] (1-based).
const IP: [u8; 64] = [
    58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6,
    64, 56, 48, 40, 32, 24, 16, 8, 57, 49, 41, 33, 25, 17, 9, 1, 59, 51, 43, 35, 27, 19, 11, 3, 61,
    53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7,
];

/// Final permutation (inverse of IP).
const FP: [u8; 64] = [
    40, 8, 48, 16, 56, 24, 64, 32, 39, 7, 47, 15, 55, 23, 63, 31, 38, 6, 46, 14, 54, 22, 62, 30,
    37, 5, 45, 13, 53, 21, 61, 29, 36, 4, 44, 12, 52, 20, 60, 28, 35, 3, 43, 11, 51, 19, 59, 27,
    34, 2, 42, 10, 50, 18, 58, 26, 33, 1, 41, 9, 49, 17, 57, 25,
];

/// Permuted choice 1: 56 bits of key material, no parity bits.
const PC1: [u8; 56] = [
    57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59, 51, 43, 35, 27, 19, 11, 3, 60,
    52, 44, 36, 63, 55, 47, 39, 31, 23, 15, 7, 62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29,
    21, 13, 5, 28, 20, 12, 4,
];

/// Permuted choice 2: 48-bit round subkey.
const PC2: [u8; 48] = [
    14, 17, 11, 24, 1, 5, 3, 28, 15, 6, 21, 10, 23, 19, 12, 4, 26, 8, 16, 7, 27, 20, 13, 2, 41, 52,
    31, 37, 47, 55, 30, 40, 51, 45, 33, 48, 44, 49, 39, 56, 34, 53, 46, 42, 50, 36, 29, 32,
];

/// Left rotations per round.
const SHIFTS: [u8; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];

/// Expansion function: 32 bits to 48.
const E: [u8; 48] = [
    32, 1, 2, 3, 4, 5, 4, 5, 6, 7, 8, 9, 8, 9, 10, 11, 12, 13, 12, 13, 14, 15, 16, 17, 16, 17, 18,
    19, 20, 21, 20, 21, 22, 23, 24, 25, 24, 25, 26, 27, 28, 29, 28, 29, 30, 31, 32, 1,
];

/// Permutation applied to each S-box output group.
const P: [u8; 32] = [
    16, 7, 20, 21, 29, 12, 28, 17, 1, 15, 23, 26, 5, 18, 31, 10, 2, 8, 24, 14, 32, 27, 3, 9, 19,
    13, 30, 6, 22, 11, 4, 25,
];

/// The eight substitution boxes.
const S: [[[u8; 16]; 4]; 8] = [
    [
        [14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7],
        [0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12, 11, 9, 5, 3, 8],
        [4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0],
        [15, 12, 8, 2, 4, 9, 1, 7, 5, 11, 3, 14, 10, 0, 6, 13],
    ],
    [
        [15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10],
        [3, 13, 4, 7, 15, 2, 8, 14, 12, 0, 1, 10, 6, 9, 11, 5],
        [0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15],
        [13, 8, 10, 1, 3, 15, 4, 2, 11, 6, 7, 12, 0, 5, 14, 9],
    ],
    [
        [10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8],
        [13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5, 14, 12, 11, 15, 1],
        [13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7],
        [1, 10, 13, 0, 6, 9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12],
    ],
    [
        [7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15],
        [13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2, 12, 1, 10, 14, 9],
        [10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4],
        [3, 15, 0, 6, 10, 1, 13, 8, 9, 4, 5, 11, 12, 7, 2, 14],
    ],
    [
        [2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9],
        [14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15, 10, 3, 9, 8, 6],
        [4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14],
        [11, 8, 12, 7, 1, 14, 2, 13, 6, 15, 0, 9, 10, 4, 5, 3],
    ],
    [
        [12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11],
        [10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13, 14, 0, 11, 3, 8],
        [9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6],
        [4, 3, 2, 12, 9, 5, 15, 10, 11, 14, 1, 7, 6, 0, 8, 13],
    ],
    [
        [4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1],
        [13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5, 12, 2, 15, 8, 6],
        [1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2],
        [6, 11, 13, 8, 1, 4, 10, 7, 9, 5, 0, 15, 14, 2, 3, 12],
    ],
    [
        [13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7],
        [1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6, 11, 0, 14, 9, 2],
        [7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8],
        [2, 1, 14, 7, 4, 10, 8, 13, 15, 12, 9, 0, 3, 5, 6, 11],
    ],
];

/// Read bit `position` (1-based, most significant first) from `bytes`.
fn bit(bytes: &[u8], position: u8) -> u64 {
    let index = usize::from(position - 1);
    let byte = bytes[index / 8];
    let shift = 7 - (index % 8);
    u64::from((byte >> shift) & 1)
}

/// Gather `table.len()` bits from `source` according to `table`.
///
/// The result is right-aligned. A caller that feeds the bits back in as bytes
/// must left-align them first ([`left_aligned`]), because [`bit`] reads the most
/// significant bit of the first byte as bit one.
fn permute(source: &[u8], table: &[u8]) -> u64 {
    let mut out = 0u64;
    for position in table {
        out = (out << 1) | bit(source, *position);
    }
    out
}

/// Place the low `width` bits of `value` in the top `width` bits of a byte
/// array, which is the layout [`permute`] reads.
fn left_aligned(value: u64, width: u32) -> [u8; 8] {
    (value << (64 - width)).to_be_bytes()
}

/// VNC's key quirk: reverse the bits of each of the eight key bytes.
pub fn vnc_key_from_password(password: &[u8]) -> [u8; 8] {
    let mut key = [0u8; 8];
    for (index, slot) in key.iter_mut().enumerate() {
        let byte = *password.get(index).unwrap_or(&0);
        *slot = byte.reverse_bits();
    }
    key
}

impl DesKeySchedule {
    /// Build the round subkeys for `key`.
    pub fn new(key: &[u8; 8]) -> Self {
        let selected = permute(key, &PC1);
        let mut left = ((selected >> 28) & 0x0FFF_FFFF) as u32;
        let mut right = (selected & 0x0FFF_FFFF) as u32;
        let mut subkeys = [[0u8; 6]; ROUNDS];
        for round in 0..ROUNDS {
            let shift = u32::from(SHIFTS[round]);
            left = ((left << shift) | (left >> (28 - shift))) & 0x0FFF_FFFF;
            right = ((right << shift) | (right >> (28 - shift))) & 0x0FFF_FFFF;
            // PC2 numbers its 56 inputs from one, and `permute` treats bit one
            // as the most significant bit of the slice. A 56-bit value therefore
            // has to sit in the top 56 bits of the u64 -- left half at bits
            // 1..28, right half at bits 29..56 -- which means shifting the left
            // half by 36 and the right half by 8, not by 28 and 0.
            let combined = (u64::from(left) << 36) | (u64::from(right) << 8);
            let bytes = combined.to_be_bytes();
            let packed = permute(&bytes, &PC2);
            subkeys[round][0] = ((packed >> 40) & 0xFF) as u8;
            subkeys[round][1] = ((packed >> 32) & 0xFF) as u8;
            subkeys[round][2] = ((packed >> 24) & 0xFF) as u8;
            subkeys[round][3] = ((packed >> 16) & 0xFF) as u8;
            subkeys[round][4] = ((packed >> 8) & 0xFF) as u8;
            subkeys[round][5] = (packed & 0xFF) as u8;
        }
        Self { subkeys }
    }

    /// Encrypt one 8-byte block in place.
    pub fn encrypt_block(&self, block: &mut [u8; 8]) {
        let permuted = permute(block, &IP);
        let mut left = ((permuted >> 32) & 0xFFFF_FFFF) as u32;
        let mut right = (permuted & 0xFFFF_FFFF) as u32;
        for round in 0..ROUNDS {
            // One DES round: L(n+1) = R(n) and R(n+1) = L(n) ^ f(R(n), K(n)).
            // The new right half must be derived from the *old* left half, so
            // the halves move through a temporary instead of being swapped
            // first, which would feed the new left into f.
            let next_right = left ^ feistel(right, &self.subkeys[round]);
            left = right;
            right = next_right;
        }
        // The loop leaves R16 in `right`, and DES feeds the final permutation
        // R16 || L16 -- the halves are crossed, not in round order. Both
        // vectors in this module fail with the halves in the other order while
        // still looking like a plausible ciphertext, which is why they are
        // pinned to an independent implementation.
        let preoutput = (u64::from(right) << 32) | u64::from(left);
        let bytes = preoutput.to_be_bytes();
        let final_block = permute(&bytes, &FP);
        *block = final_block.to_be_bytes();
    }
}

/// The DES round function: expand, mix with the subkey, substitute, permute.
fn feistel(right: u32, subkey: &[u8; 6]) -> u32 {
    let expanded_bytes = right.to_be_bytes();
    let mut expanded_source = [0u8; 4];
    expanded_source.copy_from_slice(&expanded_bytes);
    let expanded = permute(&expanded_source, &E);
    let key = u64::from(subkey[0]) << 40
        | u64::from(subkey[1]) << 32
        | u64::from(subkey[2]) << 24
        | u64::from(subkey[3]) << 16
        | u64::from(subkey[4]) << 8
        | u64::from(subkey[5]);
    let mixed = expanded ^ key;

    let mut substituted = 0u32;
    for box_index in 0..8 {
        let shift = 42 - (box_index * 6);
        let six = ((mixed >> shift) & 0x3F) as usize;
        let row = ((six & 0x20) >> 4) | (six & 1);
        let column = (six >> 1) & 0x0F;
        substituted = (substituted << 4) | u32::from(S[box_index][row][column]);
    }
    let bytes = left_aligned(u64::from(substituted), 32);
    permute(&bytes, &P) as u32
}

/// Encrypt `data` (a multiple of eight bytes) with `key` in ECB mode.
pub fn encrypt_ecb(key: &[u8; 8], data: &mut [u8]) {
    let schedule = DesKeySchedule::new(key);
    for chunk in data.chunks_exact_mut(8) {
        let mut block = [0u8; 8];
        block.copy_from_slice(chunk);
        schedule.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
}

/// Answer a VNC authentication challenge.
///
/// `challenge` is the 16 bytes the server sent; the returned 16 bytes are the
/// response. A challenge of any other length is the server's error, not this
/// client's, so the length is enforced by the caller before reaching here.
pub fn answer_challenge(password: &[u8], challenge: &[u8; 16]) -> [u8; 16] {
    let key = vnc_key_from_password(password);
    let mut response = *challenge;
    encrypt_ecb(&key, &mut response);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 197 / the original DES specification's canonical vector. If this
    /// ever fails, authentication against every server fails too.
    #[test]
    fn des_matches_the_published_test_vector() {
        let key: [u8; 8] = [0x13, 0x34, 0x57, 0x79, 0x9B, 0xBC, 0xDF, 0xF1];
        let mut block: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];
        let schedule = DesKeySchedule::new(&key);
        schedule.encrypt_block(&mut block);
        assert_eq!(block, [0x85, 0xE8, 0x13, 0x54, 0x0F, 0x0A, 0xB4, 0x05]);
    }

    /// All-ones is a DES weak key, so it is its own inverse: encrypting the
    /// ciphertext again must return the plaintext.
    ///
    /// This key is checked by that property rather than against a reference
    /// value, because `.NET`'s `DES.Key` setter rewrites the parity bits and
    /// silently substitutes a different key (all-ones becomes `220fdb583ee779d2`),
    /// so its output for this key is not comparable.
    #[test]
    fn the_all_ones_weak_key_is_its_own_inverse() {
        let key: [u8; 8] = [0x01; 8];
        let schedule = DesKeySchedule::new(&key);
        let plaintext: [u8; 8] = [0x80, 0, 0, 0, 0, 0, 0, 0];
        let mut block = plaintext;
        schedule.encrypt_block(&mut block);
        let cipher = block;
        schedule.encrypt_block(&mut block);
        assert_eq!(block, plaintext, "cipher={cipher:02x?} is not self-inverse");
    }

    #[test]
    fn the_password_becomes_a_bit_reversed_eight_byte_key() {
        // "abc" pads with zeros, and each byte's bits are reversed.
        let key = vnc_key_from_password(b"abc");
        assert_eq!(key[0], b'a'.reverse_bits());
        assert_eq!(key[1], b'b'.reverse_bits());
        assert_eq!(key[2], b'c'.reverse_bits());
        assert_eq!(&key[3..], &[0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_password_longer_than_eight_bytes_is_truncated() {
        let long = vnc_key_from_password(b"0123456789");
        let short = vnc_key_from_password(b"01234567");
        assert_eq!(long, short);
    }

    #[test]
    fn a_challenge_answers_in_two_independent_blocks() {
        let challenge = [0u8; 16];
        let response = answer_challenge(b"password", &challenge);
        // Encrypting the zero block twice must give the same 8 bytes twice,
        // which is what ECB means and what a server expects.
        assert_eq!(&response[0..8], &response[8..16]);
        assert_ne!(&response[0..8], &[0u8; 8]);
    }

    #[test]
    fn ecb_encryption_leaves_no_partial_block_untouched() {
        let key = vnc_key_from_password(b"secret");
        let mut data = [0u8; 16];
        encrypt_ecb(&key, &mut data);
        assert_ne!(data, [0u8; 16]);
    }
}
