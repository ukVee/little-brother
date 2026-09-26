#!/usr/bin/env bash
# Hold each ILED condition long enough to eyeball the tablet's front bezel
# through a phone camera.
#
# Looking straight at the emitter beats measuring light it reflects off a wall
# by orders of magnitude, so this is the more sensitive test of the two -- but
# only for 850nm parts.  A 940nm emitter (also common for Windows Hello) is
# close to invisible to a phone with a decent IR-cut filter, so a negative here
# is suggestive, not conclusive.  The sweep is the quantitative answer.
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "must run as root (writes /dev/i2c-2)" >&2; exit 1; }

ILED=/home/ukv/cement-ir/iledctl.py
HOLD=${HOLD:-15}

echo "Point a phone camera at the front bezel, above the screen, centre-right."
echo "Watch for a faint purple/white dot. Each step holds ${HOLD}s."
echo

for cond in "off:0x00" "ILED A on:0x04" "ILED B on @ 2mA:0x40" "ILED B on @ 16mA:0x70"; do
  name=${cond%:*}
  val=${cond##*:}
  python3 "$ILED" set "$val" >/dev/null
  printf '>>> %-22s %s\n' "$name" "$(python3 "$ILED" get)"
  sleep "$HOLD"
done

python3 "$ILED" set 0x00 >/dev/null
echo "restored: $(python3 "$ILED" get)"
