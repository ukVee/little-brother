#!/usr/bin/env bash
# Regenerate frame-r2-t65-05.expected.u16 from the committed raw frame using
# lib/ip3y.py — the de-facto reference implementation.
#
# Bit-identity with CURRENT Python behaviour only: the August verification
# against the reference ipu3-unpack decoder used a validate_ip3y.py that was
# never saved, so re-verifying against upstream means writing that harness
# again. Needs python3 + numpy. Run from anywhere.
set -euo pipefail
here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd -- "$here/../.." && pwd)"
python3 - "$here" "$root" <<'EOF'
import sys, pathlib
sys.path.insert(0, str(pathlib.Path(sys.argv[2]) / "lib"))
import ip3y
import numpy as np
here = pathlib.Path(sys.argv[1])
raw = (here / "frame-r2-t65-05.raw").read_bytes()
assert len(raw) == ip3y.FRAME_BYTES, f"raw frame is {len(raw)} bytes, want {ip3y.FRAME_BYTES}"
px = ip3y.unpack(raw)
(here / "frame-r2-t65-05.expected.u16").write_bytes(
    np.ascontiguousarray(px, dtype="<u2").tobytes())
print(f"wrote {here / 'frame-r2-t65-05.expected.u16'} ({len(px)} samples)")
EOF
