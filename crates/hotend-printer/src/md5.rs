//! MD5 (RFC 1321), for one purpose: a sliced archive carries the MD5 of its G-code in
//! `Metadata/plate_<n>.gcode.md5`, and the check compares the two. MD5 is not a defence
//! against a forger, who writes both; a mismatch is a cheap sign that the G-code was
//! changed after the slicer wrote it. Nothing else may use this module.

/// Per-round shift amounts.
const SHIFTS: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// The constants of the algorithm: `floor(2^32 * abs(sin(i + 1)))`.
fn constant(index: usize) -> u32 {
    ((index as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32
}

/// A running MD5.
pub(crate) struct Md5 {
    state: [u32; 4],
    block: [u8; 64],
    filled: usize,
    length: u64,
    constants: [u32; 64],
}

impl Md5 {
    pub(crate) fn new() -> Self {
        let mut constants = [0u32; 64];
        for (index, slot) in constants.iter_mut().enumerate() {
            *slot = constant(index);
        }
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            block: [0; 64],
            filled: 0,
            length: 0,
            constants,
        }
    }

    fn compress(&mut self) {
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(self.block.chunks_exact(4)) {
            *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for (round, (constant, shift)) in self.constants.iter().zip(SHIFTS).enumerate() {
            let (mixed, at) = match round / 16 {
                0 => ((b & c) | (!b & d), round),
                1 => ((d & b) | (!d & c), (5 * round + 1) % 16),
                2 => (b ^ c ^ d, (3 * round + 5) % 16),
                _ => (c ^ (b | !d), (7 * round) % 16),
            };
            let sum = a
                .wrapping_add(mixed)
                .wrapping_add(*constant)
                .wrapping_add(words[at]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(shift));
        }
        for (state, add) in self.state.iter_mut().zip([a, b, c, d]) {
            *state = state.wrapping_add(add);
        }
    }

    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        while !bytes.is_empty() {
            let take = (64 - self.filled).min(bytes.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled == 64 {
                self.compress();
                self.filled = 0;
            }
        }
    }

    /// The digest as 32 lower-case hex digits.
    pub(crate) fn finish(mut self) -> String {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.block[56..].copy_from_slice(&bits.to_le_bytes());
        self.compress();
        let digest: Vec<u8> = self
            .state
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        crate::job::hex(&digest)
    }
}

#[cfg(test)]
mod tests {
    use super::Md5;

    fn md5(bytes: &[u8]) -> String {
        let mut running = Md5::new();
        // In uneven pieces, so the block handling is exercised.
        for piece in bytes.chunks(7) {
            running.update(piece);
        }
        running.finish()
    }

    #[test]
    fn the_test_vectors_of_rfc_1321_hold() {
        for (text, digest) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ] {
            assert_eq!(md5(text.as_bytes()), digest, "{text:?}");
        }
    }
}
