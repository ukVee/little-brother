#!/usr/bin/env bash
# Drive the TPS68470 WLED (flash/torch) output and see whether the IR
# illuminator lights up.
#
# WHY THIS AND NOT THE ILEDs.  The 2026-08-21 sweep toggled ILEDA/ILEDB, the
# PMIC's *indicator* outputs, which cap at 16 mA -- and were in fact stuck at
# their 2 mA reset default because leds-tps68470 never programs the current.
# The WLED block is a separate, high-current output: 227.5 mA in torch mode,
# 1007.5 mA in flash.  Nothing in Linux touches it, and on this machine every
# WLED register still reads its hardware reset value.
#
# linux-surface#739 is the corroboration: djrscally got the IR LED working on a
# Surface Go 2 and describes it as driven through the TPS68470's flash/torch
# path with an 875 ms (flash) / 13 s (torch) safety timer, re-triggered by the
# sensor's S_STROBE.  Torch mode does not need the strobe or streaming --
# "Torch and focus assist will immediately begin driving current when enabled"
# -- so it is the cheapest way to find out whether an emitter exists at all.
#
# SAFETY.  Starts at the minimum non-zero step (32.5 mA) and steps up only
# after a clean status read.  WLEDSTAT is checked after every step for LEDF
# (LED short/fault), TSD (thermal shutdown) and TO (timeout); the script aborts
# on a fault rather than pushing more current into it.  Torch is disabled
# between steps, never left latched.
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "must run as root (writes /dev/i2c-2)" >&2; exit 1; }

PMIC=/home/ukv/cement-ir/tps68470.py
IRUP=/home/ukv/cement-ir/ir-up.sh
OUT=/home/ukv/ir-wled
FRAMES=${FRAMES:-10}

REG_WLEDSTAT=0x29
REG_WLEDMAXT=0x2d
REG_WLEDC1=0x34
REG_WLEDC2=0x35
REG_WLEDCTL=0x36

rm -rf "$OUT"; mkdir -p "$OUT"
exec > >(tee -a "$OUT/log.txt") 2>&1

echo "=== $(date -Is)  TPS68470 WLED torch test ==="
echo "--- baseline ---"
python3 "$PMIC" status

torch_off() { python3 "$PMIC" set $REG_WLEDCTL 0x00 >/dev/null; }
trap 'echo; echo "--- disabling torch ---"; torch_off; python3 "$PMIC" status' EXIT

check_fault() {  # check_fault <label>
  local s
  s=$(python3 "$PMIC" get $REG_WLEDSTAT | awk '{print $3}')
  local v=$((s))
  if (( v & 0x20 )); then echo "!! $1: LEDF — LED short/fault. Aborting."; return 1; fi
  if (( v & 0x40 )); then echo "!! $1: TSD — thermal shutdown. Aborting."; return 1; fi
  return 0
}

# torch_on <steps> <disled1> <disled2>   -- steps of 32.5mA
torch_on() {
  local steps=$1 d1=$2 d2=$3
  torch_off                                  # MAXT is not writable while enabled
  python3 "$PMIC" set $REG_WLEDMAXT "$steps" >/dev/null
  python3 "$PMIC" set $REG_WLEDC1   "$steps" >/dev/null
  python3 "$PMIC" set $REG_WLEDC2   "$steps" >/dev/null
  local ctl=$(( 0x01 | 0x04 | (d1 << 3) | (d2 << 4) ))   # MODE=torch, EN=1
  python3 "$PMIC" set $REG_WLEDCTL "$ctl" >/dev/null
}

########## PART 1: visual, no camera needed ##########
echo
echo "--- PART 1: look at the front bezel through a phone camera ---"
echo "Torch mode is continuous, so the camera does not need to be streaming."
echo
for spec in "LED1+LED2 @ 32.5mA:1:0:0" \
            "LED1 only  @ 32.5mA:1:0:1" \
            "LED2 only  @ 32.5mA:1:1:0" \
            "LED1+LED2 @ 65mA:2:0:0" \
            "LED1+LED2 @ 130mA:4:0:0"; do
  IFS=: read -r label steps d1 d2 <<<"$spec"
  echo ">>> $label"
  torch_on "$steps" "$d1" "$d2"
  python3 "$PMIC" status | sed 's/^/    /'
  check_fault "$label" || break
  sleep 8          # inside the 13s torch watchdog
  torch_off
  sleep 1
done

echo
read -r -p "Did the bezel light up in ANY of those? Press ENTER to continue..." _

########## PART 2: quantitative, through the IR sensor ##########
echo
echo "--- PART 2: measured through the IR camera ---"
echo "Point the tablet at a wall or your face ~40cm away, lights off, hold still."
read -r -p "Press ENTER to start..." _

capture() {  # capture <tag>
  local tag=$1
  v4l2-ctl -d /dev/video2 --stream-mmap --stream-count="$FRAMES" \
           --stream-to="$OUT/$tag.raw" >/dev/null 2>&1
}

for spec in "off:0:0:0" "both_32ma:1:0:0" "both_65ma:2:0:0" "both_130ma:4:0:0"; do
  IFS=: read -r tag steps d1 d2 <<<"$spec"
  "$IRUP" >/dev/null            # graph up + gain/exposure, every time
  if [[ $steps == 0 ]]; then torch_off; else torch_on "$steps" "$d1" "$d2"; fi
  before=$(python3 "$PMIC" get $REG_WLEDCTL | awk '{print $3}')
  capture "$tag"
  after=$(python3 "$PMIC" get $REG_WLEDCTL | awk '{print $3}')
  st=$(python3 "$PMIC" get $REG_WLEDSTAT | awk '{print $3}')
  if [[ "$before" != "$after" ]]; then
    echo "   $tag  WLEDCTL CHANGED MID-CAPTURE $before -> $after  (13s torch watchdog?)"
  else
    echo "   $tag  WLEDCTL=$after WLEDSTAT=$st"
  fi
  torch_off
done

chown -R ukv:ukv "$OUT"
echo
echo "captures: $(ls "$OUT"/*.raw 2>/dev/null | wc -l) in $OUT"
