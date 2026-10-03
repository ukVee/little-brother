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
#   auth sufficient pam_howdy.so [workaround=...]         <- face auth
#   auth include    system-auth | login                   <- password, unchanged
#
# 'sufficient' means success ends auth successfully and FAILURE FALLS THROUGH to
# the password below it. 'optional' on the trigger means it can never block.
# Neither line can lock you out; the worst case is that face auth never fires and
# you type your password as before.
#
# The howdy lines MUST go ABOVE the include, never below it. Arch's system-auth
# ends its auth stack with `[default=die] pam_faillock.so authfail`, so a failed
# password kills the stack outright -- a `sufficient pam_howdy.so` placed after
# the include would never be reached (slice 010).
#
#   cement-ir pam status [service]          what is wired up (no root needed)
#   cement-ir pam list                      one line per service, machine-readable
#   cement-ir pam enable [service]          default service: sudo
#   cement-ir pam disable [service]         restores the .pre-howdy backup
#   cement-ir pam set-device [/dev/videoN]  point howdy's config.ini at the loopback
#
# The helper's source is ~/dotfiles/src/system/cement-ir/howdy-ir-pam (since
# slice 006). Since 2026-10-03 it is installed as a ROOT-OWNED COPY by
# `sudo dotfiles-deploy-system`, not a bombadil symlink: root runs it on every
# face auth, and a link into ~/dotfiles let any ukv process edit it (garden
# services/notes/001). `cement-ir doctor` verifies it. There is no
# install-helper anymore.
#
# PAM SERVICE FILES ARE THE ONE /etc EXCEPTION TO THE DOTFILES RULE (slice 010).
# Locked decision 10 says files in /etc come from ~/dotfiles (bombadil-linked
# then; root-owned copies via dotfiles-deploy-system since 2026-10-03). /etc/pam.d/* stays
# hand-edited with a .pre-howdy backup, for two different reasons per file:
#   - /etc/pam.d/sudo IS in pacman's backup array, so a symlink would collect a
#     .pacnew on every sudo upgrade (the original exception).
#   - /etc/pam.d/gtklock is package-owned and is NOT in any backup array
#     (`pacman -Qii gtklock` -> "Backup Files : None"), so pacman overwrites it
#     silently on upgrade. Hand-editing makes that a fail-SAFE silent loss of
#     face auth (the stock `auth include login` returns and the password still
#     works), which is strictly better than the bombadil alternative: if a
#     render ever produced a missing/empty file, PAM falls through to
#     /etc/pam.d/other -> pam_deny -> the lock screen stops accepting anything.
#     `cement-ir doctor` is what makes the silent revert visible.
set -uo pipefail

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"

HELPER=/usr/local/bin/howdy-ir-pam
HELPER_LOG=/var/log/howdy-ir-pam.log

source "$LIB/pam-policy.sh"

cmd_list() {
    local svc f want got
    for svc in $(pam_enabled_services); do
        f=$PAM_DIR/$svc
        want=$(pam_workaround_for "$svc")
        got=$(sed -n 's/.*pam_howdy\.so[[:space:]]*//p' "$f" | head -1)
        printf '%s\topts=%s\tpolicy=%s\tbackup=%s\tpacman=%s\n' \
            "$svc" "${got:-none}" \
            "$([[ ${got:-} == "${want:-}" ]] && echo ok || echo "mismatch(want ${want:-none})")" \
            "$([[ -f $f.pre-howdy ]] && echo yes || echo no)" \
            "$(pam_pkg_state "$f")"
    done
}

cmd_status() {
    local svc=${1:-sudo} f=$PAM_DIR/${1:-sudo}
    echo "=== $PAM_DIR/$svc ==="
    if [[ -r $f ]]; then
        grep -nE "howdy|pam_exec|system-auth|include" "$f" | sed 's/^/  /' \
            || echo "  (no howdy lines)"
        echo "  policy: $(pam_workaround_for "$svc" | sed 's/^$/no workaround (graphical or unknown)/')"
        case $(pam_pkg_state "$f") in
            backup)    echo "  pacman: in the owning package's backup array -- an upgrade leaves a .pacnew, the edit survives" ;;
            revert:*)  echo "  pacman: !! owned by $(pam_pkg_state "$f" | cut -d: -f2) and NOT a backup file -- an upgrade OVERWRITES it silently (doctor watches for this)" ;;
            unowned)   echo "  pacman: unowned, nothing will touch it" ;;
        esac
    else
        echo "  not readable as $(id -un)"
    fi
    [[ -f $f.pre-howdy ]] && echo "  backup present: $f.pre-howdy"
    echo "=== helper (a root-owned copy of ~/dotfiles/src/system/cement-ir/howdy-ir-pam) ==="
    if why=$(deployed_state "$HELPER" cement-ir/howdy-ir-pam); then
        grep -q 'irfaced trigger' "$HELPER" \
            && echo "  $HELPER: $why" \
            || echo "  !! $HELPER does not carry the irfaced trigger call"
    else
        echo "  !! $HELPER: $why"
    fi
    echo "=== services carrying face auth ==="
    cmd_list | sed 's/^/  /' || echo "  (none)"
}

cmd_enable() {
    local svc=${1:-sudo} f=$PAM_DIR/${1:-sudo} n
    # The never-touch check comes FIRST, before the root check: `pam enable su`
    # should tell you it will never happen, not that you forgot sudo.
    for n in "${PAM_NEVER[@]}"; do
        [[ $svc == "$n" ]] && die "refusing to touch $PAM_DIR/$svc -- it is on the never-touch list (the lockout surface). See PAM_NEVER in lib/pam-policy.sh for why."
    done
    # Root is needed to write the real auth path, not to write a test dir.
    [[ $PAM_DIR == /etc/pam.d ]] && need_root pam
    [[ -f $f ]] || die "$f does not exist (do not create a PAM service file to add face auth to it)"
    if [[ $PAM_DIR == /etc/pam.d && ! -x $HELPER ]]; then
        die "$HELPER is not installed -- run: sudo dotfiles-deploy-system"
    fi
    touch "$HELPER_LOG" 2>/dev/null && chmod 644 "$HELPER_LOG" 2>/dev/null || true
    if grep -q pam_howdy "$f"; then
        echo "pam_howdy already present in $f:"; grep -n howdy "$f"; return 0
    fi

    local wa howdy_line
    wa=$(pam_workaround_for "$svc")
    howdy_line="auth       sufficient pam_howdy.so${wa:+ $wa}"

    cp -a "$f" "$f.pre-howdy"
    echo "backed up $f -> $f.pre-howdy"
    local tmp; tmp=$(mktemp)
    {
        head -1 "$f" | grep -q '^#%PAM' && head -1 "$f"
        echo "# --- howdy face auth (added by cement-ir pam enable $svc) ---"
        echo "# optional: triggers the face-capture daemon; can never block authentication"
        echo "auth       optional   pam_exec.so seteuid /usr/local/bin/howdy-ir-pam"
        echo "# sufficient: success ends auth; FAILURE falls through to the password below"
        [[ -z $wa ]] && echo "# no workaround=: pam_howdy(8)'s 'native' needs a real terminal, which a"
        [[ -z $wa ]] && echo "# graphical consumer does not have, and 'native-input' injects an untargeted"
        [[ -z $wa ]] && echo "# synthetic Enter through /dev/uinput. See pam_workaround_for() in lib/pam.sh."
        echo "$howdy_line"
        echo "# --- end howdy ---"
        if head -1 "$f" | grep -q '^#%PAM'; then tail -n +2 "$f"; else cat "$f"; fi
    } > "$tmp"
    cat "$tmp" > "$f"
    rm -f "$tmp"
    echo
    echo "=== $f now reads ==="; cat -n "$f"
    echo
    case $(pam_pkg_state "$f") in
      revert:*)
        echo "!! $PAM_DIR/$svc is owned by $(pam_pkg_state "$f" | cut -d: -f2) and is NOT in its backup"
        echo "   array, so a package upgrade will OVERWRITE this file silently -- no .pacnew, no"
        echo "   warning, face auth just stops. That is the fail-safe direction (the password still"
        echo "   works), and 'cement-ir doctor' is what makes it visible. Re-run 'pam enable $svc'"
        echo "   after any $(pam_pkg_state "$f" | cut -d: -f2) upgrade."
        echo ;;
    esac
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
    local f=$PAM_DIR/${1:-sudo}
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
    list)           shift; cmd_list "$@" ;;
    enable)         shift; cmd_enable "$@" ;;
    disable)        shift; cmd_disable "$@" ;;
    set-device)     shift; cmd_set_device "$@" ;;
    *) die "pam: expected status|list|enable|disable|set-device, got '$1'" ;;
esac
