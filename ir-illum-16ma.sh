#!/usr/bin/env bash
# Retest the TPS68470 iLEDs as the IR illuminator, this time with ILED B's
# current actually programmed.
#
# Why this differs from ir-illum-test.sh (2026-08-21, clean negative):
#   That run toggled brightness via the LED class, which ONLY flips an enable
#   bit.  leds-tps68470 programs ILED B's current from a device property
#   ("ti,ledb-current") that int3472 never supplies, so CTRLB stayed at its
#   power-on default of 0 = 2 mA.  Confirmed live: reg 0x28 read 0x00.
#   So the whole previous sweep compared "off" against "on at the 2 mA floor".
#
# Prediction to falsify: b-only at 2 mA measured +2.9 of 1023 over baseline,
# which is inside the ~±3 noise floor.  At 16 mA (8x the current) a real
# emitter should land near +20..25, well clear of noise.  If 16 mA still shows
# nothing, ILED B is not wired to an emitter and this line of enquiry is dead.
#
# Interleaves conditions and repeats them so ambient drift cannot masquerade as
# signal, and reads the register back after every capture -- a silent reset
# during streaming produced a false negative once already.
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "must run as root (writes /dev/i2c-2)" >&2; exit 1; }

ILED=/home/ukv/cement-ir/iledctl.py
IRUP=/home/ukv/cement-ir/ir-up.sh
OUT=/home/ukv/ir-illum2
REPEATS=${REPEATS:-3}
FRAMES=${FRAMES:-10}

# name:regvalue -- OFF interleaved between every live condition
CONDITIONS=(off:0x00 b_2ma:0x40 b_16ma:0x70 a_on:0x04)

rm -rf "$OUT"; mkdir -p "$OUT"
LOG="$OUT/log.txt"

echo "=== ILED sweep, $REPEATS repeats x ${#CONDITIONS[@]} conditions, $FRAMES frames ===" | tee "$LOG"
echo "start reg: $(python3 "$ILED" get)" | tee -a "$LOG"

for rep in $(seq 1 "$REPEATS"); do
  for cond in "${CONDITIONS[@]}"; do
    name=${cond%%:*}
    val=${cond##*:}
    tag="$(printf 'r%d-%s' "$rep" "$name")"

    # Graph up + gain/exposure re-applied every time: streaming can power-cycle
    # the sensor, and the controls do not survive that.
    "$IRUP" >/dev/null

    python3 "$ILED" set "$val" >/dev/null
    before=$(python3 "$ILED" get)

    v4l2-ctl -d /dev/video2 --stream-mmap --stream-count="$FRAMES" \
             --stream-to="$OUT/$tag.raw" >/dev/null 2>&1

    after=$(python3 "$ILED" get)

    if [[ "$before" != "$after" ]]; then
      echo "!! $tag REGISTER CHANGED DURING CAPTURE: $before -> $after" | tee -a "$LOG"
    else
      echo "$tag  $after" | tee -a "$LOG"
    fi
  done
done

python3 "$ILED" set 0x00 >/dev/null
echo "restored: $(python3 "$ILED" get)" | tee -a "$LOG"
chown -R ukv:ukv "$OUT"
ls -l "$OUT"
