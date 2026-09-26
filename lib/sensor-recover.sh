#!/usr/bin/env bash
# Recover a wedged ov7251 without rebooting.
#
# Symptom: v4l2-ctl exits 0 with zero bytes, VIDIOC_STREAMON returns EINVAL, and
# dmesg shows:
#     ov7251 ...: ov7251_write_reg: write reg error -121: reg=103, val=1
#     ov7251 ...: error during global init
# -121 is EREMOTEIO -- the sensor is not answering I2C at all, so the driver
# cannot even write the software-reset register.
#
# The ov7251's rails AND its enable/powerdown/reset GPIOs all come from the
# TPS68470 (gpiochip1: gpio-5 enable, gpio-7 powerdown, gpio-9 reset). We drive
# that same PMIC's WLED boost for the illuminator, and a 40s hold at 130mA per
# LED left GSTAT reporting PWR_FLAG|OVP|UVLO -- an undervoltage lockout on the
# rail that powers the sensor. Re-probing the driver re-runs power-up and
# toggles reset, which is the cheap fix.
set -uo pipefail
source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"
[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }

DEV_ID=i2c-INT347E:00
DRV=/sys/bus/i2c/drivers/ov7251

probe() {
  "$LIB/ir-up.sh" >/dev/null 2>&1 || true
  local node
  node=$("$LIB/ir-up.sh" 2>/dev/null | sed -n 's/^ready: \(\/dev\/video[0-9]*\).*/\1/p')
  node=${node:-/dev/video2}
  rm -f /tmp/rec.raw
  timeout 12 v4l2-ctl -d "$node" --stream-mmap --stream-count=3 \
      --stream-to=/tmp/rec.raw 2>/tmp/rec.err
  local sz; sz=$(stat -c%s /tmp/rec.raw 2>/dev/null || echo 0)
  echo "  [$1] node=$node bytes=$sz"
  [[ -s /tmp/rec.err ]] && sed 's/^/      /' /tmp/rec.err
  [[ $sz -gt 0 ]]
}

echo "=== PMIC state (is a fault latched?) ==="
RT=$(rt_binary)
if [[ -n $RT ]]; then
    "$RT" torch status | sed 's/^/  /'
    echo "  clearing latched GSTAT flags"
    "$RT" torch clear-flags | sed 's/^/  /'
    # Make sure the illuminator is not left drawing current.
    "$RT" torch off >/dev/null
else
    echo "  cement-ir-rt not found -- cannot read or clear the PMIC (sudo make -C $ROOT install)"
fi

echo
echo "=== 1. before ==="
probe "before" && { echo "already working"; exit 0; }

echo
echo "=== 2. unbind + rebind ov7251 (re-runs power-up and reset) ==="
if [[ -e $DRV/$DEV_ID ]]; then
  echo -n "$DEV_ID" > "$DRV/unbind" 2>&1 && echo "  unbound" || echo "  unbind failed"
  sleep 2
  echo -n "$DEV_ID" > "$DRV/bind" 2>&1 && echo "  rebound" || echo "  bind failed"
  sleep 3
else
  echo "  $DEV_ID not bound to ov7251 (already unbound?)"
  echo -n "$DEV_ID" > "$DRV/bind" 2>/dev/null && echo "  bound" || echo "  bind failed"
  sleep 3
fi
dmesg | grep -iE "ov7251" | tail -4 | sed 's/^/  /'
probe "after rebind" && { echo; echo "RECOVERED without reboot"; exit 0; }

echo
echo "=== still wedged -- a reboot will clear it ==="
echo "Cold boot has worked every time; the graph renumbers but ir-up.sh"
echo "resolves it by entity name now."
exit 1
