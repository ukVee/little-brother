#!/usr/bin/env python3
"""Unpack IPU3 'ip3y' packed 10-bit greyscale into a 2-D array.

IPU3 packs 25 pixels into each 32-byte group as a little-endian bitstream of
25 x 10 = 250 bits, with the group's top 6 bits as padding.  It is NOT 4 px per
5 bytes -- and the wrong grouping yields the SAME 832-byte stride and the same
399360-byte frame, so every arithmetic check still passes and only the picture
reveals the error.  See garden `projects/cement-ir/notes/002` (and
`hardware/notes/001` for the stride arithmetic).

This implementation was verified pixel-for-pixel against the reference Rust
decoder (ipu3-unpack) in August 2026 -- but by a validate_ip3y.py that was never
saved, so re-verifying means writing that harness again.  Do not "optimise" this
without doing so.
"""
import numpy as np

WIDTH = 640
HEIGHT = 480
PX_PER_GROUP = 25
GROUP_BYTES = 32
GROUPS_PER_LINE = -(-WIDTH // PX_PER_GROUP)          # ceil -> 26
STRIDE = GROUPS_PER_LINE * GROUP_BYTES               # 832
FRAME_BYTES = STRIDE * HEIGHT                        # 399360

_WEIGHTS = (1 << np.arange(10)).astype(np.uint16)


def unpack(buf, width=WIDTH, height=HEIGHT):
    """bytes -> uint16 array (height, width) of 10-bit samples."""
    a = np.frombuffer(buf, dtype=np.uint8, count=FRAME_BYTES)
    a = a.reshape(height, GROUPS_PER_LINE, GROUP_BYTES)

    # LSB-first within each byte, so the 32 bytes read as one LE bitstream.
    bits = np.unpackbits(a, axis=2, bitorder="little")        # (h, groups, 256)
    bits = bits[:, :, : PX_PER_GROUP * 10]                    # drop 6 pad bits
    bits = bits.reshape(height, GROUPS_PER_LINE, PX_PER_GROUP, 10)

    px = (bits * _WEIGHTS).sum(axis=3, dtype=np.uint16)       # (h, groups, 25)
    return px.reshape(height, GROUPS_PER_LINE * PX_PER_GROUP)[:, :width]


def frame_at(buf, index):
    off = index * FRAME_BYTES
    return unpack(buf[off:off + FRAME_BYTES])
