#!/usr/bin/env bash
# Cement the IR camera fix on archtablito so it survives reboots AND kernel
# updates.  Run on the tablet:  sudo ~/cement-ir/cement-ir.sh
#
# ---------------------------------------------------------------------------
# WHAT IS ACTUALLY BROKEN
#
# linux-surface 6.19.8 ships drivers/media/i2c/dw9719.c WITHOUT the upstream
# stable-tagged fix d7fe0d53b2a8 ("media: dw9719: Add back the I2C device id
# table", Cc: stable # for v6.19 and later).  Without an id_table the VCM cannot
# bind, ov8865's async sub-notifier waits forever, cio2_notifier_complete() never
# runs, no pad links are created, and ALL THREE cameras — including the IR one —
# are invisible.
#
# THIS SCRIPT IS A STOPGAP, NOT THE FIX.  The real fix is linux-surface shipping
# the upstream commit.  Report it; this only holds the line locally until they do.
#
# Why the previous attempt did not survive reboot:
#   - apply.sh did `insmod ./dw9719.ko` from a home directory — installs nothing.
#   - /etc/modules-load.d/dw9719.conf says just "dw9719", so at boot modprobe
#     loads the STOCK broken module out of /lib/modules and squats the name.
# Verified: loaded srcversion 7EDAAC55.. (stock, 0 i2c aliases) vs the patched
# build 1458708E.. (1 alias).
#
# The fix here registers the patched source with DKMS, which installs into
# /lib/modules/<ver>/updates/dkms/ — that path takes precedence over kernel/, so
# `modprobe dw9719` resolves to the patched build — and rebuilds automatically on
# every kernel update, which a bare .ko copy would not.
# ---------------------------------------------------------------------------
set -euo pipefail

NAME=dw9719-fix
VER=1.0
SRC_DIR=/usr/src/${NAME}-${VER}
# Where the patched source currently lives (from the earlier session's work).
STAGED=${STAGED:-/home/ukv/dw9719fix}

[[ $EUID -eq 0 ]] || { echo "must run as root: sudo $0" >&2; exit 1; }
[[ -f $STAGED/dw9719.c ]] || { echo "no patched source at $STAGED/dw9719.c" >&2; exit 1; }

command -v dkms >/dev/null || {
    echo "dkms is not installed. Install it first:  pacman -S dkms" >&2
    exit 1
}

# Refuse to proceed if the staged source lacks the fix — otherwise this would
# cement the broken driver, which is worse than doing nothing.
if ! grep -q 'i2c_device_id' "$STAGED/dw9719.c"; then
    echo "ERROR: $STAGED/dw9719.c has no i2c_device_id table — that is the unpatched" >&2
    echo "       source. Cementing it would persist the bug. Aborting." >&2
    exit 1
fi

echo "### staging source into $SRC_DIR"
rm -rf "$SRC_DIR"
install -Dm644 "$STAGED/dw9719.c" "$SRC_DIR/dw9719.c"

cat > "$SRC_DIR/Makefile" <<'EOF'
obj-m := dw9719.o
EOF

cat > "$SRC_DIR/dkms.conf" <<EOF
PACKAGE_NAME="$NAME"
PACKAGE_VERSION="$VER"
BUILT_MODULE_NAME[0]="dw9719"
DEST_MODULE_LOCATION[0]="/updates/dkms"
MAKE[0]="make -C \${kernel_source_dir} M=\${dkms_tree}/\${PACKAGE_NAME}/\${PACKAGE_VERSION}/build modules"
CLEAN="make -C \${kernel_source_dir} M=\${dkms_tree}/\${PACKAGE_NAME}/\${PACKAGE_VERSION}/build clean"
AUTOINSTALL="yes"
EOF

echo "### removing any previous dkms registration"
dkms remove -m "$NAME" -v "$VER" --all 2>/dev/null || true

echo "### dkms add / build / install"
dkms add -m "$NAME" -v "$VER"
dkms build -m "$NAME" -v "$VER"
dkms install -m "$NAME" -v "$VER" --force

echo "### swapping the running module for the patched one"
rmmod dw9719 2>/dev/null || true
depmod -a
modprobe dw9719

echo
echo "### verification — these must agree"
echo -n "loaded srcversion : "; cat /sys/module/dw9719/srcversion
echo -n "dkms   srcversion : "; modinfo -F srcversion dw9719
echo -n "i2c alias present : "; modinfo dw9719 | grep -c '^alias:.*i2c:dw9719' || true
echo -n "VCM bound         : "
ls -l /sys/bus/i2c/devices/i2c-INT347A:00-VCM/driver 2>/dev/null && echo OK || echo "STILL UNBOUND"

echo
echo "### media graph — each sensor should now have 1 link"
media-ctl -p -d /dev/media0 2>/dev/null | grep -E 'ov5693|ov8865|ov7251' || true

echo
echo "Done. Reboot and re-run the verification block to prove it actually persists —"
echo "the whole point of this script is surviving a cold boot, so do not record it"
echo "as working until you have seen it come up clean after one."
