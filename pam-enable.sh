#!/usr/bin/env bash
# Enable face auth for sudo ONLY. Reversible; see pam-disable.sh.
#
# KEEP A ROOT SHELL OPEN ON ANOTHER TTY BEFORE RUNNING THIS (sudo -i).
# howdy-next's own installer says the same. A broken /etc/pam.d/sudo means you
# cannot sudo to fix /etc/pam.d/sudo.
#
# Placement matters:
#   auth optional   pam_exec.so ...howdy-ir-pam   <- starts the IR feeder
#   auth sufficient pam_howdy.so workaround=native <- face auth
#   auth include    system-auth                    <- password, unchanged
#
# 'sufficient' means success ends auth successfully, and FAILURE FALLS THROUGH
# to the password below it. 'optional' on the feeder means it can never block.
# Neither line can lock you out; the worst case is that face auth never fires
# and you type your password as before.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }

SVC=${1:-sudo}
F=/etc/pam.d/$SVC
[[ -f $F ]] || { echo "$F does not exist" >&2; exit 1; }

install -m 755 /home/ukv/cement-ir/howdy-ir-pam /usr/local/bin/howdy-ir-pam

if grep -q pam_howdy "$F"; then
  echo "pam_howdy already present in $F:"; grep -n "howdy" "$F"; exit 0
fi

cp -a "$F" "$F.pre-howdy"
echo "backed up $F -> $F.pre-howdy"

tmp=$(mktemp)
{
  head -1 "$F" | grep -q '^#%PAM' && head -1 "$F"
  cat <<'EOF'
# --- howdy face auth (added by pam-enable.sh) ---
# optional: starts the IR feeder; can never block authentication
auth       optional   pam_exec.so seteuid /usr/local/bin/howdy-ir-pam
# sufficient: success ends auth; FAILURE falls through to the password below
auth       sufficient pam_howdy.so workaround=native
# --- end howdy ---
EOF
  if head -1 "$F" | grep -q '^#%PAM'; then tail -n +2 "$F"; else cat "$F"; fi
} > "$tmp"
cat "$tmp" > "$F"
rm -f "$tmp"

echo
echo "=== $F now reads ==="
cat -n "$F"
echo
echo "TEST IT AT THE TABLET, NOT OVER SSH -- abort_if_ssh=true means face auth"
echo "never fires in an ssh session, so a remote test proves nothing."
echo
echo "Kill switches, in increasing order of severity:"
echo "  sudo howdy disable true      # keeps models, stops face auth"
echo "  sudo $0 --off                # or: pam-disable.sh"
echo "  restore: cp $F.pre-howdy $F"
