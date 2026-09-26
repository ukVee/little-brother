#!/usr/bin/env bash
# cement-ir dkms -- the dw9719 carry, which is what makes any camera appear.
#
#   cement-ir dkms verify     (no root) are we running the patched module?
#   cement-ir dkms install    (root) register the patched source with DKMS
#
# ---------------------------------------------------------------------------
# WHAT IS ACTUALLY BROKEN
#
# linux-surface 6.19.8 ships drivers/media/i2c/dw9719.c WITHOUT the upstream
# stable-tagged fix d7fe0d53b2a8 ("media: dw9719: Add back the I2C device id
# table", Cc: stable # for v6.19 and later). Without an id_table the VCM cannot
# bind, ov8865's async sub-notifier waits forever, cio2_notifier_complete() never
# runs, no pad links are created, and ALL THREE cameras -- including the IR one --
# are invisible.
#
# THIS IS A STOPGAP, NOT THE FIX. The real fix is linux-surface shipping the
# upstream commit. On this device the carry is permanent by decision (on-device
# only: no upstream reports, no kernel patches) -- see
# journal/decisions/decision-cement-ir-to-projects.md, and os/kernel/notes/003.
#
# Why the pre-DKMS attempt did not survive a reboot:
#   - apply.sh did `insmod ./dw9719.ko` from a home directory -- installs nothing.
#   - /etc/modules-load.d/dw9719.conf says just "dw9719", so at boot modprobe
#     loads the STOCK broken module out of /lib/modules and squats the name.
# Verified: loaded srcversion 7EDAAC55.. (stock, 0 i2c aliases) vs the patched
# build 1458708E.. (1 alias).
#
# DKMS installs into /lib/modules/<ver>/updates/dkms/, which takes precedence
# over kernel/, so `modprobe dw9719` resolves to the patched build -- and it
# rebuilds on every kernel update, which a bare .ko copy would not.
# ---------------------------------------------------------------------------
set -uo pipefail

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"

NAME=dw9719-fix
VER=1.0
SRC_DIR=/usr/src/${NAME}-${VER}
# Where the patched source lives. NOT in this repo yet: losing it means
# re-deriving the patch, which slice 008 of the milestone is meant to settle.
STAGED=${STAGED:-/home/ukv/dw9719fix}

cmd_verify() {
    local rc=0
    echo -n "loaded srcversion : "; cat /sys/module/dw9719/srcversion 2>/dev/null || { echo "MODULE NOT LOADED"; rc=1; }
    echo -n "dkms   srcversion : "; modinfo -F srcversion dw9719 2>/dev/null || { echo "NOT FOUND"; rc=1; }
    echo -n "i2c alias present : "; modinfo dw9719 2>/dev/null | grep -c '^alias:.*i2c:dw9719' || rc=1
    echo -n "VCM bound         : "
    if [[ -e /sys/bus/i2c/devices/i2c-INT347A:00-VCM/driver ]]; then
        readlink -f /sys/bus/i2c/devices/i2c-INT347A:00-VCM/driver
    else
        echo "STILL UNBOUND"; rc=1
    fi
    echo "media graph links :"
    media-ctl -p -d /dev/media0 2>/dev/null | grep -E 'ov5693|ov8865|ov7251' | sed 's/^/  /' || true
    # All three must agree: loaded == dkms == has the i2c alias. A mismatch means
    # the stock module squatted the name again, which is every camera silently
    # disappearing -- the failure mode that produced one confidently-wrong garden
    # note that stood for three months.
    return $rc
}

cmd_install() {
    need_root dkms
    [[ -f $STAGED/dw9719.c ]] || die "no patched source at $STAGED/dw9719.c"
    have dkms || die "dkms is not installed: pacman -S dkms"
    # Refuse to cement the BROKEN driver -- that would be worse than doing nothing.
    grep -q 'i2c_device_id' "$STAGED/dw9719.c" \
        || die "$STAGED/dw9719.c has no i2c_device_id table -- that is the unpatched source. Aborting."

    echo "### staging source into $SRC_DIR"
    rm -rf "$SRC_DIR"
    install -Dm644 "$STAGED/dw9719.c" "$SRC_DIR/dw9719.c"
    printf 'obj-m := dw9719.o\n' > "$SRC_DIR/Makefile"
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
    echo "### verification -- these must agree"
    cmd_verify
    echo
    echo "Reboot and re-run \`cement-ir dkms verify\` to prove it persists -- the whole"
    echo "point is surviving a cold boot, so do not record it as working until you have"
    echo "seen it come up clean after one."
}

case ${1:-verify} in
    verify)  cmd_verify ;;
    install) cmd_install ;;
    *) die "dkms: expected verify|install, got '$1'" ;;
esac
