#!/usr/bin/env bash
# One dark-room session, three tests, one sudo.
#
#   1. POSITIVE CONTROL  -- a handheld IR remote pointed into the lens.
#      This project has produced two false negatives already (gain silently
#      reverting to minimum; sudo -n silently failing so all four conditions
#      ran in one LED state).  Without a source we KNOW is emitting, a null
#      result from the iLEDs is unfalsifiable.  If the remote does not show up,
#      the measurement chain is broken and every other number here is void.
#
#   2. VISUAL HOLD       -- same as ir-illum-hold.sh but in the dark, where a
#      weak emitter is not washed out.
#
#   3. QUANTITATIVE SWEEP -- interleaved, repeated, with register readback.
#
# Everything lands in ~/ir-dark/ owned by ukv.
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "must run as root (writes /dev/i2c-2)" >&2; exit 1; }

ILED=/home/ukv/cement-ir/iledctl.py
IRUP=/home/ukv/cement-ir/ir-up.sh
OUT=/home/ukv/ir-dark
FRAMES=${FRAMES:-10}
REPEATS=${REPEATS:-3}

rm -rf "$OUT"; mkdir -p "$OUT"
LOG="$OUT/log.txt"
exec > >(tee -a "$LOG") 2>&1

echo "=== $(date -Is) ==="
echo "initial reg: $(python3 "$ILED" get)"
echo

# Resolve the sensor subdev the same way ir-up.sh does -- the node number is
# not stable across boots, and hardcoding it is how you end up reading gain
# off the wrong device and trusting the answer.
SUBDEV=$(media-ctl -p -d /dev/media0 2>/dev/null \
         | sed -n '/entity [0-9]*: ov7251/,/^- entity/p' \
         | sed -n 's#.*device node name \(/dev/v4l-subdev[0-9]*\).*#\1#p' | head -1)
[[ -n ${SUBDEV:-} ]] || { echo "could not resolve the ov7251 subdev" >&2; exit 1; }
echo "sensor subdev: $SUBDEV"

gain_now() {
  v4l2-ctl -d "$SUBDEV" --get-ctrl analogue_gain,exposure 2>/dev/null \
    | tr '\n' ' ' | sed 's/  */ /g'
}

capture() {  # capture <tag>
  local tag=$1
  "$IRUP" >/dev/null
  local before after g_before g_after
  before=$(python3 "$ILED" get)
  g_before=$(gain_now)
  v4l2-ctl -d /dev/video2 --stream-mmap --stream-count="$FRAMES" \
           --stream-to="$OUT/$tag.raw" >/dev/null 2>&1
  after=$(python3 "$ILED" get)
  g_after=$(gain_now)

  # Both of this project's false negatives were invisible state changes: gain
  # reverting to its minimum of 16, and the LED state never actually moving.
  # Read both back on the far side of the capture, not just before it.
  local flag=""
  [[ "$before" != "$after" ]] && flag+=" !!LED-CHANGED($before -> $after)"
  [[ "$g_before" != "$g_after" ]] && flag+=" !!GAIN-CHANGED($g_before -> $g_after)"
  [[ "$g_after" == *"analogue_gain: 16 "* ]] && flag+=" !!GAIN-AT-MINIMUM"
  echo "   ${tag}  [$after] [$g_after]${flag}"
}

########## 1. positive control ##########
# A phone flashlight is a white LED and emits little IR, so this does NOT
# calibrate the sensor at 850/940nm.  What it does prove is that the chain
# end-to-end works on the night: gain applied, stream captured, decode correct,
# analysis sensitive to a real change in the scene.  That is what the control
# is for -- the sensor's response to ordinary room light is already established
# from the lampshade frame, so wavelength sensitivity is not what is in doubt.
#
# Step 1c is the interesting one: flesh blocks visible light far more than it
# blocks near-IR, so light reaching the sensor through a hand is IR-enriched.
# A strong reading there means no tight IR bandpass filter is fitted, i.e. the
# sensor would comfortably see a working emitter.
echo "--- TEST 1: positive control (phone flashlight) ---"
python3 "$ILED" set 0x00 >/dev/null
echo "Baseline first: lights off, nothing pointed at the camera."
read -r -p "Press ENTER to capture the baseline..." _
capture control_baseline

echo
echo "Now shine the phone flashlight into the front camera from ~30cm."
read -r -p "Shining? Press ENTER (keep it there)..." _
capture control_flashlight

echo
echo "Now press the flashlight flat against the back of your hand, and hold the"
echo "hand ~10cm from the camera so only light THROUGH your hand reaches it."
read -r -p "In position? Press ENTER (hold still)..." _
capture control_through_hand
echo "Done, you can put the phone down."
echo

########## 2. visual hold ##########
echo "--- TEST 2: visual, phone camera on the front bezel ---"
echo "Lights off. Point your phone camera at the bezel above the screen."
for cond in "off:0x00" "ILED A:0x04" "ILED B 2mA:0x40" "ILED B 16mA:0x70"; do
  python3 "$ILED" set "${cond##*:}" >/dev/null
  printf '>>> %-16s %s\n' "${cond%:*}" "$(python3 "$ILED" get)"
  sleep 12
done
python3 "$ILED" set 0x00 >/dev/null
echo
read -r -p "Saw anything? Press ENTER to start the sweep..." _
echo

########## 3. quantitative sweep ##########
echo "--- TEST 3: sweep, ${REPEATS}x interleaved, ${FRAMES} frames each ---"
echo "Point the camera at a wall/your face ~40cm away and keep the scene still."
read -r -p "Press ENTER to start (takes a couple of minutes)..." _
for rep in $(seq 1 "$REPEATS"); do
  for cond in off:0x00 b_2ma:0x40 b_16ma:0x70 a_on:0x04; do
    python3 "$ILED" set "${cond##*:}" >/dev/null
    capture "r${rep}-${cond%%:*}"
  done
done

python3 "$ILED" set 0x00 >/dev/null
echo
echo "restored: $(python3 "$ILED" get)"
chown -R ukv:ukv "$OUT"
echo "done -- $(ls "$OUT"/*.raw | wc -l) captures in $OUT"
