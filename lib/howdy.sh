#!/usr/bin/env bash
# cement-ir howdy <howdy args> -- run a howdy command with the IR feeder live.
#
#   sudo cement-ir howdy add       # enrol a model
#   sudo cement-ir howdy test
#   sudo cement-ir howdy list
#
# Howdy reads V4L2 via OpenCV, which cannot decode the ov7251's ip3y (IPU3-packed
# 10-bit greyscale), so irface.py unpacks frames and republishes them as 8-bit
# GREY on the loopback. It also drives the IR illuminator, without which the
# detector finds nothing at all -- every torch-off frame scored zero.
#
# CRITICAL: the feeder must already be producing before howdy opens the device.
# With exclusive_caps=1 the node advertises capture capability only while a
# producer holds it open, and OpenCV gives up on the first failed read rather than
# retrying. So this waits for a real frame instead of sleeping and hoping.
#
# This is the enrolment-time counterpart of the PAM helper, and the reason it
# survived the August consolidation: it turns the torch off on EVERY exit path,
# belt-and-braces over irface.py's own cleanup. An emitter left latched is
# invisible, draws current, and 40 s at 130 mA is enough to raise OVP+UVLO on
# this PMIC -- which takes the sensor down with it.
set -uo pipefail

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"
[[ $EUID -eq 0 ]] || die "'howdy' needs root:  sudo $ROOT/cement-ir howdy $*"

DEV=${DEV:-/dev/video20}
CUR=${CUR:-1}          # 32.5mA -- best YuNet score; higher clips and scores worse
GAIN=${GAIN:-128}
DURATION=${DURATION:-90}

have howdy || die "howdy is not installed"
[[ -e $DEV ]] || die "$DEV missing -- run: sudo $ROOT/cement-ir loopback install"

# The ov7251 wedges after a feeder run often enough that recovering by hand every
# time is not viable. Symptom: v4l2-ctl exits 0 with zero bytes, STREAMON returns
# EINVAL, and the sensor stops answering I2C (EREMOTEIO). Unbinding/rebinding the
# driver re-runs power-up and clears it; it is a no-op when capture already works,
# so this is cheap. Demoting it from routine step to fault handler is slice 007.
echo "checking the IR sensor is alive..."
if ! bash "$LIB/sensor-recover.sh" >/tmp/ir-recover.log 2>&1; then
  echo "!! the IR sensor is wedged and did not recover. Log:" >&2
  tail -20 /tmp/ir-recover.log >&2
  exit 1
fi
if grep -q "RECOVERED without reboot" /tmp/ir-recover.log; then
  echo "  sensor was wedged; recovered by rebinding ov7251"
else
  echo "  sensor OK"
fi

FEEDLOG=/tmp/ir-feeder.log
: > "$FEEDLOG"
READY=/run/cement-ir/feed-ready.$$
mkdir -p /run/cement-ir && chmod 700 /run/cement-ir
"$LIB/irface.py" --gain "$GAIN" --current "$CUR" \
    feed --device "$DEV" --duration "$DURATION" --fps 10 --ready-file "$READY" \
    >"$FEEDLOG" 2>&1 &
FEEDER=$!

# Give the feeder a clear head start before probing: it has to run ir-up.sh
# (media-ctl graph setup) and open the loopback for WRITING. Probing it for reads
# every 0.5s from the first instant races that.
sleep 3
if ! kill -0 "$FEEDER" 2>/dev/null; then
  echo "!! the feeder exited immediately. Its output:" >&2
  cat "$FEEDLOG" >&2
  exit 1
fi

cleanup() {
  kill "$FEEDER" 2>/dev/null
  wait "$FEEDER" 2>/dev/null
  # Belt-and-braces over the feeder's own release: ask the one owner to
  # extinguish, whatever happened above.
  local RT; RT=$(rt_binary)
  [[ -n $RT ]] && "$RT" torch off >/dev/null 2>&1
}
trap cleanup EXIT

echo -n "waiting for the feeder to produce a frame"
ok=0
for _ in $(seq 1 40); do
  # The feeder signals its own readiness (slice 004): after the first unpacked
  # frame reaches ffmpeg it touches $READY. The old probe read the loopback
  # with python-opencv -- exactly the dependency the 2026-09-06 orphan sweep
  # deleted -- and this project no longer imports cv2 anywhere.
  if [[ -f $READY ]]; then
    ok=1; break
  fi
  if ! kill -0 "$FEEDER" 2>/dev/null; then
    echo; echo "!! the feeder died while we waited. Its output:" >&2
    cat "$FEEDLOG" >&2
    exit 1
  fi
  echo -n "."
  sleep 1
done
echo
if [[ $ok -eq 0 ]]; then
  echo "!! the feeder never signaled a frame -- not running howdy." >&2
  echo "   check: cement-ir doctor." >&2
  exit 1
fi
echo "feeder live on $DEV, signaled ready (torch ${CUR}x32.5mA, gain $GAIN)"
echo "--- howdy $* ---"
howdy "$@"
rc=$?
echo "--- howdy exited $rc ---"
exit $rc
