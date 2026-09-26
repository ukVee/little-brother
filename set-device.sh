#!/usr/bin/env bash
# Point howdy-next at the IR loopback.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
CFG=/etc/howdy/config.ini
cp -n "$CFG" "$CFG.orig" 2>/dev/null || true
sed -i 's#^device_path = .*#device_path = /dev/video20#' "$CFG"
echo "=== effective settings ==="
grep -nE "^(device_path|timeout|dark_threshold|clahe_enabled|max_height|abort_if_ssh|disabled|yunet_score_threshold|sface_threshold) " "$CFG"
