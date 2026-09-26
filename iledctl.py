#!/usr/bin/env python3
"""Read/write TPS68470 ILEDCTL (reg 0x28) directly over /dev/i2c-2.

The PMIC sits at 0x4D on i2c-2 (from its ACPI _CRS).  The kernel's int3472
driver owns that address, so I2C_SLAVE_FORCE is required.  That regmap is
REGCACHE_NONE, so direct writes and the LED class driver cannot desync.

  ILEDCTL bits:  ENA = BIT(2)          ILED A enable (no current control)
                 CTRLB = bits 5:4      ILED B current: 0=2mA 1=4mA 2=8mA 3=16mA
                 ENB = BIT(6)          ILED B enable

Usage:  iledctl.py get
        iledctl.py set 0x70
"""
import fcntl
import os
import sys

I2C_SLAVE_FORCE = 0x0706
BUS = "/dev/i2c-2"
ADDR = 0x4D
REG = 0x28


def _open():
    fd = os.open(BUS, os.O_RDWR)
    fcntl.ioctl(fd, I2C_SLAVE_FORCE, ADDR)
    return fd


def get():
    fd = _open()
    try:
        os.write(fd, bytes([REG]))
        return os.read(fd, 1)[0]
    finally:
        os.close(fd)


def put(val):
    fd = _open()
    try:
        os.write(fd, bytes([REG, val]))
    finally:
        os.close(fd)


def describe(v):
    ena = "on " if v & 0x04 else "off"
    enb = "on " if v & 0x40 else "off"
    ma = {0: 2, 1: 4, 2: 8, 3: 16}[(v >> 4) & 0x03]
    return f"0x{v:02x}  A={ena}  B={enb}  B_current={ma}mA"


if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in ("get", "set"):
        sys.exit(__doc__)
    if sys.argv[1] == "get":
        print(describe(get()))
    else:
        want = int(sys.argv[2], 0)
        put(want)
        got = get()
        print(f"wrote 0x{want:02x} -> readback {describe(got)}")
        if got != want:
            sys.exit(f"MISMATCH: wrote 0x{want:02x}, read 0x{got:02x}")
