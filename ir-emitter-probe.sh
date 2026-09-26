#!/usr/bin/env bash
# Determine which TPS68470 LED output, if any, has an emitter physically
# attached -- using the PMIC's own fault detection rather than trying to see
# faint infrared.
#
# The chip flags open and shorted loads:
#   GSTAT[5] ILEDF   -- an ILEDA/ILEDB failure occurred; ILEDCTL[3]/[7] then say
#                       which kind (0 = Open, 1 = Shorted).  READ-ONLY bits, and
#                       meaningless unless ILEDF is set.
#   GSTAT[3] OVP     -- the WLED boost hit overvoltage.  That is what an OPEN
#                       (absent/disconnected) WLED load looks like: the boost
#                       drives WLED_OUT up to its 6.0 V clamp trying to reach
#                       regulation, and flags OVP.
#   WLEDSTAT[5] LEDF -- WLED short.
#
# So: enable each output at low current, read the flags, and the chip tells us
# whether something is connected.  No dark room, no phone camera, no squinting
# at a 2 mA glow.
#
# Sequencing per the datasheet, which the previous attempt got wrong:
#   - torch is TWO writes: set MODE[1:0]='01' first, THEN set EN.  Writing
#     mode+enable in one go is what got rejected last run.
#   - VWLEDCTL[0] ENABLE must be high when enabling the WLED module.
#   - WLEDMAXT/WLEDTO cannot be written while the WLED is enabled.
#   - torch self-disables after a 13 s watchdog unless MODE is refreshed.
set -uo pipefail        # deliberately NOT -e: a rejected write is data, not a
                        # reason to abandon the run

[[ $EUID -eq 0 ]] || { echo "must run as root (writes /dev/i2c-2)" >&2; exit 1; }

P=/home/ukv/cement-ir/tps68470.py
R_GSTAT=0x01 R_ILEDCTL=0x28 R_WLEDSTAT=0x29 R_WLEDMAXT=0x2d
R_VWLEDCTL=0x31 R_WLEDC1=0x34 R_WLEDC2=0x35 R_WLEDCTL=0x36

rd() { python3 "$P" get "$1" | awk '{print $3}'; }
wr() { python3 "$P" set "$1" "$2" | sed 's/^/      /'; }

all_off() {
  python3 "$P" set $R_WLEDCTL  0x00 >/dev/null
  python3 "$P" set $R_ILEDCTL  0x00 >/dev/null
  python3 "$P" set $R_VWLEDCTL 0x38 >/dev/null   # reset value, ENABLE low
}
trap 'echo; echo "--- restoring ---"; all_off; python3 "$P" status' EXIT

echo "=== $(date -Is)  TPS68470 emitter presence probe ==="
echo
echo "--- baseline ---"
python3 "$P" status
echo
echo "--- clearing latched fault flags ---"
all_off
python3 "$P" clearflags

probe() {   # probe <label> <settle-seconds>
  local label=$1 settle=${2:-2}
  sleep "$settle"
  local g i w s
  g=$(rd $R_GSTAT); i=$(rd $R_ILEDCTL); w=$(rd $R_WLEDSTAT); s=$(rd $R_WLEDCTL)
  echo "      -> GSTAT=$g ILEDCTL=$i WLEDSTAT=$w WLEDCTL=$s"
  python3 - "$g" "$i" "$w" "$label" <<'PY'
import sys
g, i, w, label = int(sys.argv[1], 16), int(sys.argv[2], 16), int(sys.argv[3], 16), sys.argv[4]
v = []
if g & 0x20:
    v.append("ILED failure: A=%s B=%s" % (
        "shorted" if i & 0x08 else "open", "shorted" if i & 0x80 else "open"))
if g & 0x08:
    v.append("WLED boost OVP  <-- open circuit: nothing attached to that output")
if w & 0x20:
    v.append("WLED LEDF (short/fault)")
if g & 0x02 or w & 0x40:
    v.append("thermal shutdown")
print("      VERDICT[%s]: %s" % (label, "; ".join(v) if v else
      "no fault flagged -- load looks present and happy"))
PY
}

########## ILED indicator outputs ##########
echo
echo "########## ILED_A (indicator, fixed 16mA) ##########"
wr $R_ILEDCTL 0x04
probe "ILED_A" 2
python3 "$P" set $R_ILEDCTL 0x00 >/dev/null; python3 "$P" clearflags >/dev/null

echo
echo "########## ILED_B (indicator, 16mA) ##########"
wr $R_ILEDCTL 0x70          # ENB | CTRLB=11 (16mA)
probe "ILED_B" 2
python3 "$P" set $R_ILEDCTL 0x00 >/dev/null; python3 "$P" clearflags >/dev/null

########## WLED flash/torch output ##########
torch() {   # torch <steps32.5mA> <disled1> <disled2> <label>
  local steps=$1 d1=$2 d3=$3 label=$4
  python3 "$P" set $R_WLEDCTL 0x00 >/dev/null      # MAXT needs WLED disabled
  wr $R_WLEDMAXT  "$steps"
  wr $R_WLEDC1    "$steps"
  wr $R_WLEDC2    "$steps"
  wr $R_VWLEDCTL  0x39                              # 0x38 reset | ENABLE
  local mode=$(( 0x01 | (d1 << 3) | (d3 << 4) ))    # MODE=torch, EN still 0
  wr $R_WLEDCTL "$mode"
  wr $R_WLEDCTL "$(( mode | 0x04 ))"                # ...THEN EN
  probe "$label" 3
  python3 "$P" set $R_WLEDCTL 0x00 >/dev/null
  python3 "$P" clearflags >/dev/null
}

echo
echo "########## WLED torch, LED1 only @ 32.5mA ##########"
torch 1 0 1 "WLED-LED1"
echo
echo "########## WLED torch, LED2 only @ 32.5mA ##########"
torch 1 1 0 "WLED-LED2"
echo
echo "########## WLED torch, both @ 65mA ##########"
torch 2 0 0 "WLED-both-65mA"

echo
echo "If any output reported 'no fault flagged', something IS attached to it."
echo "Watch the bezel through a phone camera on this last one:"
echo
torch 4 0 0 "WLED-both-130mA-hold"
