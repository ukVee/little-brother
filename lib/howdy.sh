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

# NO sensor pre-check here. Until slice 007 this ran sensor-recover.sh -- a
# rebind -- in front of every enrolment, the same patchwork the PAM helper
# carried: a fault handler on the happy path, converting a defect into latency
# and hiding how often it fired. The clean in-process STREAMOFF (slice 006) is
# why it is not needed; `cement-ir soak 20` is the evidence; and if the sensor
# IS wedged, the feeder below fails and says so, with the handler one command
# away. Every rebind that fires is now counted (cement-ir doctor reports it).

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
  echo "   Its output:" >&2
  tail -20 "$FEEDLOG" >&2
  # This is what the pre-auth rebind used to paper over. Diagnose, then point
  # at the handler -- it is one command, and it gets counted when it fires.
  echo "   check: sudo $ROOT/cement-ir doctor" >&2
  echo "   if the sensor is wedged (STREAMON EINVAL / i2c -121 in dmesg):" >&2
  echo "     sudo $ROOT/cement-ir recover   # the fault handler, no longer a routine step" >&2
  exit 1
fi
echo "feeder live on $DEV, signaled ready (torch ${CUR}x32.5mA, gain $GAIN)"
echo "--- howdy $* ---"
howdy "$@"
rc=$?
echo "--- howdy exited $rc ---"
exit $rc
