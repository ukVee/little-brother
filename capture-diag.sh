#!/usr/bin/env bash
# The IR capture pipeline streamed fine all evening and has now wedged: v4l2-ctl
# exits 0, writes zero bytes, and says nothing on stderr. The media graph is
# still correctly linked and formatted, so this looks like driver state rather
# than configuration -- most likely from terminating v4l2-ctl mid-stream when
# the feeder's duration expired.
#
# Escalates gently: look, then re-set the graph, then power-cycle the sensor by
# reloading its module. Stops at the first thing that produces frames.
set -uo pipefail
[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }

cap() {  # cap <label>
  local out=/tmp/capdiag.raw
  rm -f $out
  /home/ukv/cement-ir/ir-up.sh >/dev/null 2>&1 || true
  timeout 12 v4l2-ctl -d /dev/video2 --stream-mmap --stream-count=3 \
      --stream-to=$out 2>/tmp/capdiag.err
  local rc=$? sz
  sz=$(stat -c%s $out 2>/dev/null || echo 0)
  echo "  [$1] exit=$rc bytes=$sz $( [[ $sz -gt 0 ]] && echo '<-- WORKS' )"
  [[ -s /tmp/capdiag.err ]] && sed 's/^/      /' /tmp/capdiag.err
  [[ $sz -gt 0 ]]
}

echo "=== kernel log: camera-related, most recent ==="
dmesg | grep -iE "ipu3|cio2|imgu|ov7251|dw9719|int3472|csi" | tail -20 | sed 's/^/  /'

echo
echo "=== 1. capture as-is ==="
cap "as-is" && { echo "already working"; exit 0; }

echo
echo "=== 2. re-set the graph explicitly ==="
SENSOR=$(media-ctl -p -d /dev/media0 2>/dev/null | sed -n 's/^- entity [0-9]*: \(ov7251 [0-9-]*\).*/\1/p' | head -1)
media-ctl -d /dev/media0 -l "\"$SENSOR\":0 -> \"ipu3-csi2 2\":0 [0]" 2>&1 | sed 's/^/  /'
sleep 1
media-ctl -d /dev/media0 -l "\"$SENSOR\":0 -> \"ipu3-csi2 2\":0 [1]" 2>&1 | sed 's/^/  /'
cap "relinked" && exit 0

echo
echo "=== 3. power-cycle the sensor (reload ov7251) ==="
modprobe -r ov7251 2>&1 | sed 's/^/  /'
sleep 2
modprobe ov7251 2>&1 | sed 's/^/  /'
sleep 3
cap "module reloaded" && exit 0

echo
echo "=== still broken. Newest kernel messages: ==="
dmesg | tail -25 | sed 's/^/  /'
exit 1
