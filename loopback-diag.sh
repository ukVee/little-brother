#!/usr/bin/env bash
# Why can nothing read /dev/video20 while the feeder is writing to it?
#
# The feeder reports "fed 2697 frames" yet every reader fails. That is exactly
# the shape of a hidden error: howdy-ir.sh probed with ffmpeg >/dev/null 2>&1,
# so whatever it said was thrown away. This runs the same probes with all output
# visible, plus a second reader (v4l2-ctl) to tell a v4l2 problem from an
# ffmpeg-specific one.
set -uo pipefail
[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }

DEV=/dev/video20
FEEDLOG=/tmp/ir-feeder-diag.log
: > "$FEEDLOG"

echo "=== BEFORE: device state with no producer ==="
v4l2-ctl -d "$DEV" --all 2>&1 | grep -iE "Card type|Driver name|Format Video|Width/Height|Pixel Format" | sed 's/^/  /'

echo
echo "=== starting feeder ==="
/home/ukv/cement-ir/irface.py --gain 128 --current 1 \
    feed --device "$DEV" --duration 45 --fps 10 >"$FEEDLOG" 2>&1 &
FEEDER=$!
trap 'kill $FEEDER 2>/dev/null; python3 /home/ukv/cement-ir/tps68470.py set 0x36 0x00 >/dev/null 2>&1' EXIT
sleep 6

if ! kill -0 "$FEEDER" 2>/dev/null; then
  echo "!! feeder already dead:"; cat "$FEEDLOG"; exit 1
fi
echo "  feeder alive (pid $FEEDER)"

echo
echo "=== who has the device open? (as root, so fds are visible) ==="
fuser -v "$DEV" 2>&1 | sed 's/^/  /'
ls -l /proc/*/fd 2>/dev/null | grep -l video20 >/dev/null 2>&1
for p in $(pgrep -f "ffmpeg.*video20"); do
  echo "  ffmpeg pid $p: $(tr '\0' ' ' < /proc/$p/cmdline)"
done

echo
echo "=== DURING: device state with the producer running ==="
v4l2-ctl -d "$DEV" --all 2>&1 | grep -iE "Card type|Format Video|Width/Height|Pixel Format" | sed 's/^/  /'

echo
echo "=== reader 1: v4l2-ctl (errors VISIBLE) ==="
timeout 10 v4l2-ctl -d "$DEV" --stream-mmap --stream-count=1 --stream-to=/tmp/diag.raw
echo "  exit=$?  size=$(stat -c%s /tmp/diag.raw 2>/dev/null || echo none)"

echo
echo "=== reader 2: ffmpeg (errors VISIBLE) ==="
timeout 10 ffmpeg -loglevel info -f v4l2 -i "$DEV" -frames:v 1 -y /tmp/diag.png 2>&1 | tail -12
echo "  exit=$?  size=$(stat -c%s /tmp/diag.png 2>/dev/null || echo none)"

echo
echo "=== feeder log so far ==="
cat "$FEEDLOG" | sed 's/^/  /'
chown ukv:ukv /tmp/diag.raw /tmp/diag.png 2>/dev/null
