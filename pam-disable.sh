#!/usr/bin/env bash
# Remove face auth from a PAM service, restoring the pre-howdy backup.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
SVC=${1:-sudo}
F=/etc/pam.d/$SVC
if [[ -f $F.pre-howdy ]]; then
  cp -a "$F.pre-howdy" "$F"
  echo "restored $F from $F.pre-howdy"
else
  sed -i '/howdy/d;/# --- end howdy ---/d' "$F"
  echo "no backup found; stripped howdy lines from $F"
fi
echo "=== $F ==="; cat -n "$F"
