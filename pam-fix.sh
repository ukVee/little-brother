#!/usr/bin/env bash
# Repair an already-installed howdy PAM line: pam_exec needs `seteuid`.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
F=/etc/pam.d/${1:-sudo}
install -m 755 /home/ukv/cement-ir/howdy-ir-pam /usr/local/bin/howdy-ir-pam
if grep -q "pam_exec.so seteuid" "$F"; then
  echo "already has seteuid"
else
  sed -i 's#pam_exec.so /usr/local/bin/howdy-ir-pam#pam_exec.so seteuid /usr/local/bin/howdy-ir-pam#' "$F"
  echo "added seteuid"
fi
touch /var/log/howdy-ir-pam.log && chmod 644 /var/log/howdy-ir-pam.log
echo "=== $F ==="; grep -n "howdy\|pam_exec\|system-auth" "$F"
