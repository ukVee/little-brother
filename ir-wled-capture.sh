#!/usr/bin/env bash
# The WLED outputs have real loads drawing current (no OVP at any level).  Now
# find out whether that load is the IR illuminator, by looking at it with the
# one instrument guaranteed to see its own emitter: the IR sensor.
#
# Conditions are interleaved and repeated so ambient drift cannot pass as
# signal, and WLEDCTL/gain are read back after every capture -- both of this
# project's earlier false negatives were invisible state changes.
#
# Torch self-disables after a 13 s watchdog, so a keep-alive refreshes MODE
# during the hold steps.  A capture is under 2 s, well inside the window.
set -uo pipefail

[[ $EUID -eq 0 ]] || { echo "must run as root (writes /dev/i2c-2)" >&2; exit 1; }

P=/home/ukv/cement-ir/tps68470.py
IRUP=/home/ukv/cement-ir/ir-up.sh
OUT=/home/ukv/ir-wled
FRAMES=${FRAMES:-10}
REPEATS=${REPEATS:-3}

R_WLEDMAXT=0x2d R_VWLEDCTL=0x31 R_WLEDC1=0x34 R_WLEDC2=0x35 R_WLEDCTL=0x36

rm -rf "$OUT"; mkdir -p "$OUT"
exec > >(tee -a "$OUT/log.txt") 2>&1

rd() { python3 "$P" get "$1" | awk '{print $3}'; }

torch_off() { python3 "$P" set $R_WLEDCTL 0x00 >/dev/null; }
trap 'echo; echo "--- restoring ---"; torch_off; python3 "$P" status' EXIT

torch_on() {   # torch_on <steps of 32.5mA>
  local steps=$1
  torch_off                                        # MAXT needs WLED disabled
  python3 "$P" set $R_WLEDMAXT "$steps"  >/dev/null
  python3 "$P" set $R_WLEDC1   "$steps"  >/dev/null
  python3 "$P" set $R_WLEDC2   "$steps"  >/dev/null
  python3 "$P" set $R_VWLEDCTL 0x39      >/dev/null
  python3 "$P" set $R_WLEDCTL  0x01      >/dev/null   # MODE=torch first
  python3 "$P" set $R_WLEDCTL  0x05      >/dev/null   # ...then EN
}

SUBDEV=$(media-ctl -p -d /dev/media0 2>/dev/null \
         | sed -n '/entity [0-9]*: ov7251/,/^- entity/p' \
         | sed -n 's#.*device node name \(/dev/v4l-subdev[0-9]*\).*#\1#p' | head -1)
gain_now() { v4l2-ctl -d "$SUBDEV" --get-ctrl analogue_gain,exposure 2>/dev/null | tr '\n' ' '; }

echo "=== $(date -Is)  WLED torch, measured through the IR sensor ==="
echo "sensor subdev: $SUBDEV"
echo

echo "Point the tablet at your face or a wall ~40cm away, lights off if you can,"
echo "and keep the scene still. This takes about two minutes."
read -r -p "Press ENTER to start..." _

for rep in $(seq 1 "$REPEATS"); do
  for spec in off:0 t32:1 t65:2 t130:4; do
    tag=${spec%%:*}; steps=${spec##*:}
    "$IRUP" >/dev/null
    if [[ $steps == 0 ]]; then torch_off; else torch_on "$steps"; fi
    c_before=$(rd $R_WLEDCTL); g_before=$(gain_now)
    v4l2-ctl -d /dev/video2 --stream-mmap --stream-count="$FRAMES" \
             --stream-to="$OUT/r${rep}-${tag}.raw" >/dev/null 2>&1
    c_after=$(rd $R_WLEDCTL); g_after=$(gain_now)
    flag=""
    [[ "$c_before" != "$c_after" ]] && flag+=" !!WLEDCTL $c_before->$c_after"
    [[ "$g_before" != "$g_after" ]] && flag+=" !!GAIN CHANGED"
    [[ "$g_after" == *"analogue_gain: 16 "* ]] && flag+=" !!GAIN-AT-MINIMUM"
    echo "   r${rep}-${tag}  WLEDCTL=$c_after${flag}"
    torch_off
  done
done

########## sustained hold, for eyeballing ##########
echo
echo "--- sustained hold at 130mA for 40s, with keep-alive ---"
echo "Look at the front bezel through a phone camera now."
torch_on 4
end=$((SECONDS + 40))
while (( SECONDS < end )); do
  python3 "$P" set $R_WLEDCTL 0x01 >/dev/null   # refresh MODE inside the 13s
  python3 "$P" set $R_WLEDCTL 0x05 >/dev/null   # watchdog, then re-enable
  printf '\r   held %2ds  WLEDCTL=%s  ' "$((40 - (end - SECONDS)))" "$(rd $R_WLEDCTL)"
  sleep 5
done
echo
torch_off

chown -R ukv:ukv "$OUT"
echo "captures: $(ls "$OUT"/*.raw 2>/dev/null | wc -l) in $OUT"
