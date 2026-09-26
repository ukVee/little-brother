#!/usr/bin/env bash
# cement-ir pam -- add, remove, inspect and repair face auth on a PAM service.
#
# KEEP A ROOT SHELL OPEN ON ANOTHER TTY BEFORE ENABLING OR DISABLING
# (`sudo -i` on another VT). howdy-next's own installer says the same. A broken
# /etc/pam.d/sudo means you cannot sudo to fix /etc/pam.d/sudo. This rule came
# from pam-enable.sh's header and it is not negotiable.
#
# Placement matters:
#   auth optional   pam_exec.so seteuid ...howdy-ir-pam   <- triggers the face-capture daemon
#   auth sufficient pam_howdy.so workaround=native        <- face auth
#   auth include    system-auth                           <- password, unchanged
#
# 'sufficient' means success ends auth successfully and FAILURE FALLS THROUGH to
# the password below it. 'optional' on the trigger means it can never block.
# Neither line can lock you out; the worst case is that face auth never fires and
# you type your password as before.
#
#   cement-ir pam status [service]          what is wired up (no root needed)
#   cement-ir pam enable [service]          default service: sudo
#   cement-ir pam disable [service]         restores the .pre-howdy backup
#   cement-ir pam set-device [/dev/videoN]  point howdy's config.ini at the loopback
#
# The helper itself is bombadil-sourced since slice 006
# (~/dotfiles/src/system/cement-ir/howdy-ir-pam -> /usr/local/bin/howdy-ir-pam);
# `bombadil link` is its installer, `cement-ir doctor` verifies it. There is no
# install-helper anymore.
set -uo pipefail

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"

HELPER=/usr/local/bin/howdy-ir-pam
HELPER_LOG=/var/log/howdy-ir-pam.log

cmd_status() {
    local svc=${1:-sudo} f=/etc/pam.d/${1:-sudo}
    echo "=== /etc/pam.d/$svc ==="
    if [[ -r $f ]]; then
        grep -nE "howdy|pam_exec|system-auth|include" "$f" | sed 's/^/  /' \
            || echo "  (no howdy lines)"
    else
        echo "  not readable as $(id -un)"
    fi
    [[ -f $f.pre-howdy ]] && echo "  backup present: $f.pre-howdy"
    echo "=== helper (bombadil-sourced since slice 006) ==="
    if [[ -e $HELPER ]]; then
        if [[ -L $HELPER ]]; then
            real=$(readlink -f "$HELPER")
            case $real in
                "$HOME"/dotfiles/.dots/*)
                    grep -q 'cement-ir-rt irfaced trigger' "$HELPER" \
                        && echo "  $HELPER -> $real (bombadil-managed)" \
                        || echo "  !! $HELPER does not carry the trigger call -- bombadil link"
                    ;;
                *) echo "  !! $HELPER is a symlink outside ~/dotfiles/.dots: $real" ;;
            esac
        else
            echo "  !! $HELPER is a hand-installed copy, not a bombadil symlink -- run: bombadil link"
        fi
    else
        echo "  $HELPER not installed -- run: bombadil link"
    fi
    echo "=== other services carrying face auth ==="
    grep -l pam_howdy /etc/pam.d/* 2>/dev/null | sed 's/^/  /' || echo "  (none readable)"
}

cmd_enable() {
    need_root pam
    local svc=${1:-sudo} f=/etc/pam.d/${1:-sudo}
    [[ -f $f ]] || die "$f does not exist"
    if [[ ! -x $HELPER ]]; then
        die "$HELPER is not installed -- run: bombadil link (one-time root ACL: sudo setfacl -m u:ukv:rwx /usr/local/bin)"
    fi
    touch "$HELPER_LOG" 2>/dev/null && chmod 644 "$HELPER_LOG" 2>/dev/null || true
    if grep -q pam_howdy "$f"; then
        echo "pam_howdy already present in $f:"; grep -n howdy "$f"; return 0
    fi
    cp -a "$f" "$f.pre-howdy"
    echo "backed up $f -> $f.pre-howdy"
    local tmp; tmp=$(mktemp)
    {
        head -1 "$f" | grep -q '^#%PAM' && head -1 "$f"
        cat <<'EOF'
# --- howdy face auth (added by cement-ir pam enable) ---
# optional: triggers the face-capture daemon; can never block authentication
auth       optional   pam_exec.so seteuid /usr/local/bin/howdy-ir-pam
# sufficient: success ends auth; FAILURE falls through to the password below
auth       sufficient pam_howdy.so workaround=native
# --- end howdy ---
EOF
        if head -1 "$f" | grep -q '^#%PAM'; then tail -n +2 "$f"; else cat "$f"; fi
    } > "$tmp"
    cat "$tmp" > "$f"
    rm -f "$tmp"
    echo
    echo "=== $f now reads ==="; cat -n "$f"
    echo
    echo "TEST IT AT THE TABLET, NOT OVER SSH -- abort_if_ssh=true means face auth"
    echo "never fires in an ssh session, so a remote test proves nothing."
    echo
    echo "Kill switches, in increasing order of severity:"
    echo "  sudo howdy disable true                      # keeps models, stops face auth"
    echo "  sudo $ROOT/cement-ir pam disable $svc"
    echo "  restore: cp $f.pre-howdy $f"
}

cmd_disable() {
    need_root pam
    local f=/etc/pam.d/${1:-sudo}
    if [[ -f $f.pre-howdy ]]; then
        cp -a "$f.pre-howdy" "$f"
        echo "restored $f from $f.pre-howdy"
    else
        sed -i '/howdy/d;/# --- end howdy ---/d' "$f"
        echo "no backup found; stripped howdy lines from $f"
    fi
    echo "=== $f ==="; cat -n "$f"
}

# /etc/howdy is 0750 root:root, so config.ini is not even readable as ukv.
cmd_set_device() {
    need_root pam
    local dev=${1:-/dev/video20} cfg=/etc/howdy/config.ini
    [[ -f $cfg ]] || die "$cfg does not exist"
    cp -n "$cfg" "$cfg.orig" 2>/dev/null || true
    sed -i "s#^device_path = .*#device_path = $dev#" "$cfg"
    echo "=== effective settings ==="
    grep -nE "^(device_path|timeout|dark_threshold|clahe_enabled|max_height|abort_if_ssh|disabled|yunet_score_threshold|sface_threshold) " "$cfg"
}

case ${1:-status} in
    status)         shift || true; cmd_status "$@" ;;
    enable)         shift; cmd_enable "$@" ;;
    disable)        shift; cmd_disable "$@" ;;
    set-device)     shift; cmd_set_device "$@" ;;
    *) die "pam: expected status|enable|disable|set-device, got '$1'" ;;
esac
