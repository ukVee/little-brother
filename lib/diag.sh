#!/usr/bin/env bash
# cement-ir diag -- escalating diagnostics, with all output VISIBLE.
#
#   cement-ir diag capture    the IR path streamed and has now wedged: why?
#   cement-ir diag loopback   the feeder says "fed N frames" but no reader can read
#
# Both of these exist because the August tree hid its own errors: probes ran with
# >/dev/null 2>&1, so a broken IR capture was misdiagnosed as a loopback problem
# and an ffmpeg-specific hang was misread as a dead pipeline. Diagnostics that
# discard stderr are worse than none.
set -uo pipefail

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"

diag_capture() {
    need_root diag
    cap() {  # cap <label>
      local out=/tmp/capdiag.raw rc sz
      rm -f $out
      bash "$LIB/ir-up.sh" >/dev/null 2>&1 || true
      timeout 12 v4l2-ctl -d "$(video_node)" --stream-mmap --stream-count=3 \
          --stream-to=$out 2>/tmp/capdiag.err
      rc=$?
      sz=$(stat -c%s $out 2>/dev/null || echo 0)
      echo "  [$1] exit=$rc bytes=$sz $( [[ $sz -gt 0 ]] && echo '<-- WORKS' )"
      [[ -s /tmp/capdiag.err ]] && sed 's/^/      /' /tmp/capdiag.err
      [[ $sz -gt 0 ]]
    }

    echo "=== kernel log: camera-related, most recent ==="
    dmesg | grep -iE "ipu3|cio2|imgu|ov7251|dw9719|int3472|csi" | tail -20 | sed 's/^/  /'

    echo; echo "=== 1. capture as-is ==="
    cap "as-is" && { echo "already working"; return 0; }

    echo; echo "=== 2. re-set the graph explicitly ==="
    local sensor csi
    sensor=$(media_entity_sensor)
    [[ -n $sensor ]] || die "ov7251 is not in the media graph at all -- run: cement-ir dkms verify"
    csi=$(media-ctl -p -d /dev/media0 2>/dev/null \
          | sed -n "/entity [0-9]*: $sensor/,/^- entity/p" \
          | sed -n 's/.*-> "\(ipu3-csi2 [0-9]*\)":0.*/\1/p' | head -1)
    csi=${csi:-ipu3-csi2 2}
    media-ctl -d /dev/media0 -l "\"$sensor\":0 -> \"$csi\":0 [0]" 2>&1 | sed 's/^/  /'
    sleep 1
    media-ctl -d /dev/media0 -l "\"$sensor\":0 -> \"$csi\":0 [1]" 2>&1 | sed 's/^/  /'
    cap "relinked" && return 0

    echo; echo "=== 3. power-cycle the sensor (reload ov7251) ==="
    modprobe -r ov7251 2>&1 | sed 's/^/  /'
    sleep 2
    modprobe ov7251 2>&1 | sed 's/^/  /'
    sleep 3
    cap "module reloaded" && return 0

    echo; echo "=== still broken. Newest kernel messages: ==="
    dmesg | tail -25 | sed 's/^/  /'
    echo "Next step is a rebind (cement-ir recover); a cold boot has cleared it every time."
    return 1
}

diag_loopback() {
    need_root diag
    local DEV=${1:-/dev/video20} FEEDLOG=/tmp/ir-feeder-diag.log
    : > "$FEEDLOG"

    echo "=== BEFORE: device state with no producer ==="
    echo "    (exclusive_caps=1 -- it is SUPPOSED to look format-less until fed)"
    v4l2-ctl -d "$DEV" --all 2>&1 | grep -iE "Card type|Driver name|Format Video|Width/Height|Pixel Format" | sed 's/^/  /'

    echo; echo "=== starting feeder ==="
    "$LIB/irface.py" --gain 128 --current 1 \
        feed --device "$DEV" --duration 45 --fps 10 >"$FEEDLOG" 2>&1 &
    local FEEDER=$!
    trap 'kill $FEEDER 2>/dev/null; python3 "$LIB/tps68470.py" set 0x36 0x00 >/dev/null 2>&1' EXIT
    sleep 6
    if ! kill -0 "$FEEDER" 2>/dev/null; then
        echo "!! feeder already dead:"; cat "$FEEDLOG"; return 1
    fi
    echo "  feeder alive (pid $FEEDER)"

    echo; echo "=== who has the device open? ==="
    fuser -v "$DEV" 2>&1 | sed 's/^/  /'
    local p
    for p in $(pgrep -f "ffmpeg.*${DEV##*/}"); do
        echo "  ffmpeg pid $p: $(tr '\0' ' ' < /proc/$p/cmdline)"
    done

    echo; echo "=== DURING: device state with the producer running ==="
    v4l2-ctl -d "$DEV" --all 2>&1 | grep -iE "Card type|Format Video|Width/Height|Pixel Format" | sed 's/^/  /'

    echo; echo "=== reader 1: v4l2-ctl (errors VISIBLE) ==="
    timeout 10 v4l2-ctl -d "$DEV" --stream-mmap --stream-count=1 --stream-to=/tmp/diag.raw
    echo "  exit=$?  size=$(stat -c%s /tmp/diag.raw 2>/dev/null || echo none)"

    echo; echo "=== reader 2: ffmpeg (errors VISIBLE; known to hang here -- exit 124"
    echo "                is the documented behaviour, not a new fault) ==="
    timeout 10 ffmpeg -loglevel info -f v4l2 -i "$DEV" -frames:v 1 -y /tmp/diag.png 2>&1 | tail -12
    echo "  exit=$?  size=$(stat -c%s /tmp/diag.png 2>/dev/null || echo none)"

    echo; echo "=== reader 3: OpenCV -- the one that decides whether howdy works ==="
    timeout 10 python3 -c '
import sys
try:
    import cv2
except ImportError:
    sys.exit("  python-opencv is MISSING -- this is the readiness gate that the\n"
             "  2026-09-06 orphan sweep broke. See journal/incidents/.")
c = cv2.VideoCapture(sys.argv[1]); ok, f = c.read(); c.release()
print(f"  ok={ok} frame={None if f is None else f.shape}")
sys.exit(0 if ok and f is not None else 1)' "$DEV"
    echo "  exit=$?"

    echo; echo "=== feeder log so far ==="
    sed 's/^/  /' "$FEEDLOG"
    chown ukv:ukv /tmp/diag.raw /tmp/diag.png 2>/dev/null
}

# Resolve the CIO2 node the same way ir-up.sh does; never hardcode it.
video_node() {
    bash "$LIB/ir-up.sh" 2>/dev/null | sed -n 's/^ready: \(\/dev\/video[0-9]*\).*/\1/p' | head -1
}

case ${1:-} in
    capture)  shift; diag_capture "$@" ;;
    loopback) shift; diag_loopback "$@" ;;
    *) die "diag: expected capture|loopback" ;;
esac
