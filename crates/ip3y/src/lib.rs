//! Pure IPU3 `ip3y` unpack — a Rust port of `lib/ip3y.py`, byte-identical.
//!
//! The IPU3 packs 25 pixels into each 32-byte group as a little-endian
//! bitstream of 25 × 10 = 250 bits, the group's top 6 bits padding. It is NOT
//! 4 px per 5 bytes — the wrong grouping yields the same 832-byte stride and
//! the same 399 360-byte frame, so every arithmetic check passes and only the
//! picture (or a byte-comparison test) reveals the error. See garden
//! `projects/cement-ir/notes/002` and `hardware/notes/001` for the stride
//! arithmetic.
//!
//! # Verification
//!
//! The golden-frame test (`tests/golden.rs`) asserts byte-identical output to
//! `lib/ip3y.py` on a committed real capture. That is bit-identity with
//! *current behaviour* and nothing more: the August verification against the
//! reference `ipu3-unpack` decoder used a `validate_ip3y.py` that was never
//! saved, so the Python implementation is the only reference that still
//! exists. Do not "optimise" this unpack without re-establishing that
//! reference.

pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 480;
pub const PX_PER_GROUP: usize = 25;
pub const GROUP_BYTES: usize = 32;
/// ceil(640 / 25) = 26 groups per line — 650 pixels packed per line, of which
/// the last 10 are padding past the 640-wide sensor readout.
pub const GROUPS_PER_LINE: usize = 26;
/// 26 × 32 = 832 bytes/line (the IPU3 aligns the line stride to 64).
pub const STRIDE: usize = GROUPS_PER_LINE * GROUP_BYTES;
/// 832 × 480.
pub const FRAME_BYTES: usize = STRIDE * HEIGHT;
pub const PIXELS: usize = WIDTH * HEIGHT;

/// Unpack a 399 360-byte `ip3y` frame into `WIDTH × HEIGHT` 10-bit samples.
///
/// `buf` may be longer than one frame (a multi-frame capture); only the first
/// `FRAME_BYTES` are read, matching `ip3y.unpack`'s `count=` behaviour.
pub fn unpack(buf: &[u8], out: &mut [u16]) {
    assert!(
        buf.len() >= FRAME_BYTES,
        "ip3y frame is {FRAME_BYTES} bytes, got {}",
        buf.len()
    );
    assert!(
        out.len() >= PIXELS,
        "output buffer holds {} samples, need {PIXELS}",
        out.len()
    );
    for row in 0..HEIGHT {
        for group in 0..GROUPS_PER_LINE {
            let base = row * STRIDE + group * GROUP_BYTES;
            for px in 0..PX_PER_GROUP {
                let bit = px * 10;
                let byte = base + bit / 8;
                let shift = (bit % 8) as u32;
                // Little-endian bitstream: low `shift`..8 bits of `byte`,
                // then the low bits of `byte + 1`. The last pixel of the
                // group (px 24, bits 240..249) never crosses the 32-byte
                // boundary — its top bits come from byte 31's low 2 bits.
                let v = ((buf[byte] as u16 >> shift)
                    | ((buf[byte + 1] as u16) << (8 - shift)))
                    & 0x3FF;
                let col = group * PX_PER_GROUP + px;
                if col < WIDTH {
                    out[row * WIDTH + col] = v;
                }
            }
        }
    }
}

/// Convenience: unpack the first frame of `buf` into a fresh `Vec<u16>`.
pub fn unpack_frame(buf: &[u8]) -> Vec<u16> {
    let mut out = vec![0u16; PIXELS];
    unpack(buf, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ones_packs_to_1023() {
        let buf = vec![0xFFu8; FRAME_BYTES];
        let out = unpack_frame(&buf);
        assert!(out.iter().all(|&v| v == 0x3FF));
    }

    #[test]
    fn single_bit_positions() {
        // Byte 0 bit 0 is the LSB of pixel 0.
        let mut buf = vec![0u8; FRAME_BYTES];
        buf[0] = 0x01;
        let out = unpack_frame(&buf);
        assert_eq!(out[0], 1);
        assert!(out[1..].iter().all(|&v| v == 0));

        // Byte 1 bit 0 is bit 8 of pixel 0 (the bitstream is LSB-first).
        let mut buf = vec![0u8; FRAME_BYTES];
        buf[1] = 0x01;
        let out = unpack_frame(&buf);
        assert_eq!(out[0], 0x100);
    }

    #[test]
    fn group_geometry() {
        // Pixel 24 of a group spans bits 240..249 — the top of byte 30 plus
        // the low 2 bits of byte 31. Pixel 25 is the FIRST pixel of the next
        // group (bit 0 of byte 32), not a continuation of this one.
        let mut buf = vec![0u8; FRAME_BYTES];
        buf[30] = 0x03;
        let out = unpack_frame(&buf);
        assert_eq!(out[24], 3);

        let mut buf = vec![0u8; FRAME_BYTES];
        buf[32] = 0x01;
        let out = unpack_frame(&buf);
        assert_eq!(out[25], 1);
        assert_eq!(out[24], 0);
    }
}
