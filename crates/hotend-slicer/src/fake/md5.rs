//! MD5 (RFC 1321), for the checksum entry a sliced archive carries beside its G-code.
//!
//! Written here on its own, not shared with the G-code check of the printer layer: the
//! check computes the sum its own way, so a mistake in either shows as a refused file.
//! MD5 is used because the archive's format has it, not for security.

/// The per-round left rotations.
const SHIFTS: [[u32; 4]; 4] = [
    [7, 12, 17, 22],
    [5, 9, 14, 20],
    [4, 11, 16, 23],
    [6, 10, 15, 21],
];

/// The MD5 of `bytes` as 32 upper-case hex digits, the way the real slicer writes it into
/// `Metadata/plate_<n>.gcode.md5`.
pub fn hex_upper(bytes: &[u8]) -> String {
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(bytes.len() as u64).wrapping_mul(8).to_le_bytes());
    for block in message.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (word, four) in words.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes([four[0], four[1], four[2], four[3]]);
        }
        let [mut a, mut b, mut c, mut d] = state;
        for step in 0..64u32 {
            let round = (step / 16) as usize;
            let (mixed, index) = match round {
                0 => ((b & c) | (!b & d), step),
                1 => ((d & b) | (!d & c), (5 * step + 1) % 16),
                2 => (b ^ c ^ d, (3 * step + 5) % 16),
                _ => (c ^ (b | !d), (7 * step) % 16),
            };
            // The constant of step i is floor(2^32 * |sin(i + 1)|).
            let constant = (f64::from(step + 1).sin().abs() * 4_294_967_296.0) as u32;
            let sum = a
                .wrapping_add(mixed)
                .wrapping_add(constant)
                .wrapping_add(words[index as usize]);
            let rotated = sum.rotate_left(SHIFTS[round][(step % 4) as usize]);
            (a, d, c, b) = (d, c, b, b.wrapping_add(rotated));
        }
        for (kept, add) in state.iter_mut().zip([a, b, c, d]) {
            *kept = kept.wrapping_add(add);
        }
    }
    state
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .map(|byte| format!("{byte:02X}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test suite of RFC 1321, appendix A.5.
    #[test]
    fn the_sums_of_the_rfc_are_reproduced() {
        for (text, sum) in [
            ("", "D41D8CD98F00B204E9800998ECF8427E"),
            ("a", "0CC175B9C0F1B6A831C399E269772661"),
            ("abc", "900150983CD24FB0D6963F7D28E17F72"),
            ("message digest", "F96B697D7CB7938D525A2F31AAF161D0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "C3FCD3D76192E4007DFB496CCA67E13B",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57EDF4A22BE3C955AC49DA2E2107B67A",
            ),
        ] {
            assert_eq!(hex_upper(text.as_bytes()), sum, "{text:?}");
        }
    }
}
