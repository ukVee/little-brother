#!/usr/bin/env bash
# cement-ir loopback install -- make the Howdy IR loopback survive reboots with a
# STABLE node number, and reload the module so the running system matches.
#
# Howdy stores an absolute device_path in its config, so the node must not move.
# video_nr=20 is well clear of the IPU3's own nodes, which renumbered once during
# the August work (the ov7251's CIO2 node moved from /dev/video2 to /dev/video9
# across a reboot and cost an hour of debugging a silent hang).
#
# exclusive_caps=1 makes the node advertise capture capability only while a
# producer holds it open -- which is what OpenCV wants, but also means the device
# looks absent whenever the daemon is not feeding. Do not read an absent
# /dev/video20 as a broken loopback.
#
# Since slice 006 the two conf files are bombadil-sourced from ~/dotfiles/src/system
# ({modprobe,modules-load}.d/v4l2loopback.conf); `bombadil link` is the installer
# and this verb verifies + reloads. (One-time root ACL if bombadil cannot write
# the targets: sudo setfacl -m u:ukv:rwx /etc/modprobe.d /etc/modules-load.d.)
set -uo pipefail

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"

case ${1:-install} in
  install) ;;
  *) die "loopback: expected install, got '$1'" ;;
esac
need_root loopback

missing=0
for f in /etc/modules-load.d/v4l2loopback.conf /etc/modprobe.d/v4l2loopback.conf; do
    if [[ -f $f ]]; then
        echo "  ok: $f"
    else
        echo "  MISSING: $f -- run: bombadil link"
        missing=1
    fi
done
(( missing )) && exit 1

# Reload now so the running system matches what a reboot would produce.
if lsmod | grep -q '^v4l2loopback'; then
  fuser -k /dev/video20 2>/dev/null || true
  sleep 1
  modprobe -r v4l2loopback 2>/dev/null || {
      echo "NOTE: module busy; the new options apply on next boot"; exit 0; }
fi
modprobe v4l2loopback
sleep 1
echo "=== result ==="
v4l2-ctl --list-devices 2>/dev/null | grep -A2 -i howdy || ls -l /dev/video20
