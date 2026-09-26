#!/usr/bin/env bash
# Determine whether either TPS68470 iled output is the IR illuminator.
#
#   ~/cement-ir/ir-illum-test.sh          # run as your normal user, in a DARK room
#
# Prompts for sudo once (LED brightness is root:root 0644); the timestamp cache
# covers the rest of the run. Captures stay owned by you because only the LED
# writes are elevated.
#
# Run this in genuine darkness. In a lit room ambient IR swamps the emitter and
# every condition looks the same — which is exactly how the 2026-08-21 attempt
# produced a convincing-looking false negative.
set -euo pipefail

A="/sys/class/leds/tps68470-iled_a::indicator/brightness"
B="/sys/class/leds/tps68470-iled_b::indicator/brightness"
OUT=${OUT:-$HOME/illum}
FRAMES=${FRAMES:-3}

mkdir -p "$OUT"

[[ -w /dev/video2 || -r /dev/video2 ]] || echo "warning: /dev/video2 may not be accessible" >&2

echo "priming sudo (needed only for the LED writes)"
sudo -v

for cond in "0 0" "1 0" "0 1" "1 1"; do
    read -r a b <<<"$cond"
    name="a${a}_b${b}"

    sudo sh -c "echo $a > '$A'; echo $b > '$B'"

    # Read the values back — a write that silently did nothing would otherwise
    # look identical to a working one.
    ra=$(cat "$A"); rb=$(cat "$B")
    if [[ $ra != "$a" || $rb != "$b" ]]; then
        echo "  WARNING: asked for a=$a b=$b but read back a=$ra b=$rb" >&2
    fi

    # Re-assert the graph and (critically) the gain/exposure before every capture.
    "$HOME/cement-ir/ir-up.sh" >/dev/null

    # Let the emitter settle and the sensor run a few frames at the new state.
    sleep 1

    v4l2-ctl -d /dev/video2 --stream-mmap --stream-count="$FRAMES" \
             --stream-to="$OUT/$name.raw" >/dev/null 2>&1

    # Read back AFTER streaming too. Powering the camera up brings the TPS68470
    # rails with it, and if that resets the LED then the capture was taken in a
    # different state than the one requested — which would look exactly like an
    # emitter that does nothing.
    pa=$(cat "$A"); pb=$(cat "$B")
    if [[ $pa != "$a" || $pb != "$b" ]]; then
        echo "  WARNING: LED state changed during capture: asked a=$a b=$b, after capture a=$pa b=$pb" >&2
        echo "           This capture does NOT test the requested condition." >&2
    fi

    printf '  a=%s b=%s (after: a=%s b=%s) -> %s (%s bytes)\n' "$a" "$b" "$pa" "$pb" \
           "$name.raw" "$(stat -c%s "$OUT/$name.raw")"
done

# Leave the emitter off rather than lit indefinitely.
sudo sh -c "echo 0 > '$A'; echo 0 > '$B'"

echo
echo "captures in $OUT:"
ls -l "$OUT"
echo
echo "Now decode them on the GE76 to compare means."
