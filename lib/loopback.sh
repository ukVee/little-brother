#!/usr/bin/env bash
# cement-ir loopback install -- make the Howdy IR loopback survive reboots with a
# STABLE node number.
#
# Howdy stores an absolute device_path in its config, so the node must not move.
# video_nr=20 is well clear of the IPU3's own nodes, which renumbered once during
# the August work (the ov7251's CIO2 node moved from /dev/video2 to /dev/video9
# across a reboot and cost an hour of debugging a silent hang).
#
# exclusive_caps=1 makes the node advertise capture capability only while a
# producer holds it open -- which is what OpenCV wants, but also means the device
# looks absent whenever the feeder is not running. Do not read an absent
# /dev/video20 as a broken loopback.
#
# The two config files are DATA in this repo (system/), not heredocs, so the
# installed state can be diffed against the tree. Whether they should be
# bombadil-sourced instead is slice 006's call.
set -uo pipefail

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"

case ${1:-install} in
  install) ;;
  *) die "loopback: expected install, got '$1'" ;;
esac
need_root loopback

install -m 644 "$SYSTEM/modules-load.d/v4l2loopback.conf" /etc/modules-load.d/v4l2loopback.conf
install -m 644 "$SYSTEM/modprobe.d/v4l2loopback.conf"     /etc/modprobe.d/v4l2loopback.conf
echo "installed:"
echo "  /etc/modules-load.d/v4l2loopback.conf"
echo "  /etc/modprobe.d/v4l2loopback.conf"

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
