//! Fourcc and media-bus format codes, verified against the kernel headers.

/// Little-endian fourcc, exactly `v4l2_fourcc(a,b,c,d)`.
pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

/// `V4L2_PIX_FMT_IPU3_Y10` — the IPU3 CIO2 raw 10-bit packed format
/// ("ip3y", 399 360 bytes/frame at 640x480; see `crates/ip3y`).
pub const IPU3_Y10: u32 = fourcc(b'i', b'p', b'3', b'y'); // 0x79337069

/// `V4L2_PIX_FMT_GREY` — the 8-bit gray format the v4l2loopback sink serves.
pub const GREY: u32 = fourcc(b'G', b'R', b'E', b'Y'); // 0x59455247

/// `MEDIA_BUS_FMT_Y10_1X10` — the ov7251's CSI-2 bus format on the media bus.
pub const MEDIA_BUS_FMT_Y10_1X10: u32 = 0x200a;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_codes_match_probed() {
        assert_eq!(IPU3_Y10, 0x79337069);
        assert_eq!(GREY, 0x59455247);
        assert_eq!(MEDIA_BUS_FMT_Y10_1X10, 0x200a);
    }
}
