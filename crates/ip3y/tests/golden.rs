//! Golden-frame test: the Rust unpack must be byte-identical to
//! `lib/ip3y.py` on a committed real capture.
//!
//! The frame is `frame-r2-t65-05.raw` — frame 5 of
//! `/home/ukv/ir-wled/r2-t65.raw` (2026-08-22 23:04 WLED torch sweep, run
//! `r2`, `WLEDCTL=0x25` lit, torch at 2 × 32.5 mA = 65 mA; 10 frames per
//! file). `frame-r2-t65-05.expected.u16` is the numpy reference output —
//! `np.ascontiguousarray(ip3y.unpack(raw), dtype="<u2").tobytes()` —
//! regenerable with `testdata/gen-expected.sh`.
//!
//! This asserts bit-identity with *current* behaviour only: the August
//! verification against the reference `ipu3-unpack` decoder used a
//! `validate_ip3y.py` that was never saved, so the Python implementation is
//! the only reference that still exists. Claiming more would be overclaiming.
use ip3y::*;

const RAW: &[u8] = include_bytes!("../testdata/frame-r2-t65-05.raw");
const EXPECTED: &[u8] = include_bytes!("../testdata/frame-r2-t65-05.expected.u16");

#[test]
fn byte_identical_to_lib_ip3y_py() {
    assert_eq!(RAW.len(), FRAME_BYTES, "golden raw frame size drifted");
    assert_eq!(EXPECTED.len(), PIXELS * 2, "golden expected size drifted");

    let out = unpack_frame(RAW);
    let got: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(got.len(), EXPECTED.len());
    for (i, (g, e)) in got.iter().zip(EXPECTED.iter()).enumerate() {
        assert_eq!(*g, *e, "first mismatch at output byte {i}");
    }
}
