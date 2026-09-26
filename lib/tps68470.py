#!/usr/bin/env python3
"""Read/write TPS68470 PMIC registers directly over /dev/i2c-2 (addr 0x4D).

The kernel's int3472 driver owns the address, so I2C_SLAVE_FORCE is needed.
Its regmap is REGCACHE_NONE, so direct writes cannot desync a driver cache.

The WLED block is the high-current flash/torch driver -- a different output
entirely from the ILEDA/ILEDB *indicator* pins that leds-tps68470 exposes.
Indicator pins top out at 16 mA; WLED torch goes to 227.5 mA and flash to
1007.5 mA.  On this machine the whole WLED block sits at hardware reset,
because nothing in Linux has ever programmed it.

  0x29 WLEDSTAT   S_STROBE|TSD|LEDF|TO|-|LEDHOT|LEDWARN|ENTMON
  0x2D WLEDMAXT   MAX_CUR[2:0], 32.5mA steps, max 227.5mA (torch ceiling)
                  -- cannot be written while WLED is enabled
  0x34 WLEDC1     ILED[4:0], 32.5mA steps  (LED1 current)
  0x35 WLEDC2     ILED[4:0], 32.5mA steps  (LED2 current)
  0x36 WLEDCTL    TRIG_POL|TRIG|START|DISLED2|DISLED1|EN|MODE[1:0]
                  MODE 00=flash 01=torch 10=red-eye 11=focus-assist
"""
import fcntl
import os
import sys

I2C_SLAVE_FORCE = 0x0706
BUS = "/dev/i2c-2"
ADDR = 0x4D

NAMES = {
    0x01: "GSTAT", 0x28: "ILEDCTL", 0x29: "WLEDSTAT", 0x2A: "VWLEDILIM",
    0x2D: "WLEDMAXT", 0x2F: "WLEDMAXF", 0x30: "WLEDTO", 0x31: "VWLEDCTL",
    0x34: "WLEDC1", 0x35: "WLEDC2", 0x36: "WLEDCTL",
}


def _open():
    fd = os.open(BUS, os.O_RDWR)
    fcntl.ioctl(fd, I2C_SLAVE_FORCE, ADDR)
    return fd


def get(reg):
    fd = _open()
    try:
        os.write(fd, bytes([reg]))
        return os.read(fd, 1)[0]
    finally:
        os.close(fd)


def put(reg, val):
    fd = _open()
    try:
        os.write(fd, bytes([reg, val]))
    finally:
        os.close(fd)


def decode_status(v):
    bits = []
    if v & 0x80: bits.append("S_STROBE-high")
    if v & 0x40: bits.append("TSD-THERMAL-SHUTDOWN")
    if v & 0x20: bits.append("LEDF-LED-SHORT/FAULT")
    if v & 0x10: bits.append("TO-timeout")
    if v & 0x04: bits.append("LEDHOT")
    if v & 0x02: bits.append("LEDWARN")
    return ", ".join(bits) if bits else "clean"


def decode_ctl(v):
    mode = ["flash", "torch", "red-eye", "focus-assist"][v & 0x03]
    return (f"mode={mode} en={(v >> 2) & 1} disled1={(v >> 3) & 1} "
            f"disled2={(v >> 4) & 1} start={(v >> 5) & 1}")


def decode_gstat(v):
    bits = []
    if v & 0x80: bits.append("SHORT_FLAG")
    if v & 0x40: bits.append("PWR_FLAG")
    if v & 0x20: bits.append("ILEDF")          # ILEDA and/or ILEDB failure
    if v & 0x10: bits.append("WLEDF")
    if v & 0x08: bits.append("OVP")            # WLED boost overvoltage = open LED
    if v & 0x04: bits.append("UVLO")
    if v & 0x02: bits.append("TSD_FLAG")
    if v & 0x01: bits.append("WAKE")
    return ", ".join(bits) if bits else "clean"


def decode_iled(v):
    # ILEDCTL: FAILB|ENB|CTRLB[1:0]|FAILA|ENA|-|-
    # FAILA/FAILB are READ-ONLY failure-mode bits: 0=Open, 1=Shorted.
    # They only mean anything when GSTAT.ILEDF is set.
    ma = {0: 2, 1: 4, 2: 8, 3: 16}[(v >> 4) & 0x03]
    return (f"ENA={(v >> 2) & 1} FAILA={'shorted' if v & 0x08 else 'open'}  "
            f"ENB={(v >> 6) & 1} B_current={ma}mA "
            f"FAILB={'shorted' if v & 0x80 else 'open'}")


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "set":
        reg, val = int(sys.argv[2], 0), int(sys.argv[3], 0)
        put(reg, val)
        got = get(reg)
        # Some "control" registers carry read-only status in among the R/W bits,
        # so a readback that differs is not automatically a failed write:
        #   WLEDCTL[5] START -- reflects whether LED current is actually flowing
        #   ILEDCTL[7] FAILB, ILEDCTL[3] FAILA -- read-only failure-mode bits
        ignore = {0x36: 0x20, 0x28: 0x88}.get(reg, 0x00)
        bad = (got & ~ignore) != (val & ~ignore)
        note = ""
        if reg == 0x36 and got & 0x20:
            note = "   [START=1: LED current is flowing]"
        print(f"  {NAMES.get(reg, hex(reg)):9s} wrote 0x{val:02x} -> 0x{got:02x}"
              + ("  MISMATCH" if bad else "") + note)
        sys.exit(1 if bad else 0)
    elif len(sys.argv) >= 2 and sys.argv[1] == "status":
        g, s, c, i = get(0x01), get(0x29), get(0x36), get(0x28)
        print(f"  GSTAT    0x{g:02x}  [{decode_gstat(g)}]")
        print(f"  WLEDSTAT 0x{s:02x}  [{decode_status(s)}]")
        print(f"  WLEDCTL  0x{c:02x}  [{decode_ctl(c)}]")
        print(f"  ILEDCTL  0x{i:02x}  [{decode_iled(i)}]")
        print(f"  WLEDMAXT 0x{get(0x2D):02x}  WLEDC1 0x{get(0x34):02x}  "
              f"WLEDC2 0x{get(0x35):02x}  VWLEDCTL 0x{get(0x31):02x}")
    elif len(sys.argv) >= 2 and sys.argv[1] == "clearflags":
        # GSTAT bits are write-1-to-clear.  WLEDF only clears while WLEDCTL.EN=0.
        put(0x01, 0xFE)
        g = get(0x01)
        print(f"  GSTAT after clear 0x{g:02x}  [{decode_gstat(g)}]")
    elif len(sys.argv) >= 3 and sys.argv[1] == "get":
        reg = int(sys.argv[2], 0)
        print(f"  {NAMES.get(reg, hex(reg)):9s} = 0x{get(reg):02x}")
    else:
        sys.exit(__doc__)
