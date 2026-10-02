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
# Where the patched source lives: IN THIS REPO, under git (slice 008). It used
# to be only ~/dw9719fix -- outside version control, one `rm -rf` from gone
# while the DKMS registration that depends on it stayed installed. Derived from
# $ROOT like every other path here, so moving the repo costs nothing; STAGED=
# still overrides it for a one-off build from somewhere else.
# Provenance, the patch itself and the srcversions: system/dw9719/README.md.
STAGED=${STAGED:-$ROOT/system/dw9719}

# The five conditions live in the runtime core (crates/cement-ir/src/dkms.rs),
# not here: the pacman hook and the boot unit call the INSTALLED binary, and a
# second copy of the checks in shell would be free to disagree with the one the
# hook enforces. This verb is the human-facing entry point to that one
# implementation, plus the media-graph context a human wants to see.
cmd_verify() {
    local RT; RT=$(rt_binary) || true
    if [[ -z ${RT:-} ]]; then
        die "cement-ir-rt not built or installed -- make -C $ROOT build (then sudo make install)"
    fi
    "$RT" dkms verify
    local rc=$?
    echo
    echo "media graph links :"
    media-ctl -p -d /dev/media0 2>/dev/null | grep -E 'ov5693|ov8865|ov7251' | sed 's/^/  /' || true
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
    depmod -a
    # A bound VCM cannot be swapped live. Once ipu3-cio2's notifier completes,
    # v4l2_device_register_subdev() holds a reference on the dw9719 module, so
    # rmmod is EBUSY for as long as the camera graph is up. That is the normal
    # state, not an error. This used to be `rmmod ... || true`, which hid the
    # failure and made verify blame "the stock module". Unbinding to force it
    # would tear the graph down under the face-auth path, and it would only
    # exercise the rebind ordering (garden projects/cement-ir/notes/005), not the
    # cold-boot one. So: say so, and let the reboot load the new build.
    local swapped=1
    if [[ -d /sys/module/dw9719 ]] && ! rmmod dw9719 2>/dev/null; then
        swapped=0
        echo "    the running dw9719 is pinned by the camera graph -- NOT swapped."
        echo "    The new build is installed and loads at the next boot."
    fi
    [[ $swapped = 1 ]] && modprobe dw9719

    echo
    echo "### verification -- these must agree"
    cmd_verify
    echo
    [[ $swapped = 0 ]] && echo "(The srcversion FAILs above are expected until that reboot.)"
    echo "Reboot and re-run \`cement-ir dkms verify\` to prove it persists -- the whole"
    echo "point is surviving a cold boot, so do not record it as working until you have"
    echo "seen it come up clean after one."
}

case ${1:-verify} in
    verify)  cmd_verify ;;
    install) cmd_install ;;
    *) die "dkms: expected verify|install, got '$1'" ;;
esac
