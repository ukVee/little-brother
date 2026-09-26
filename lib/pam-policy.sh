# PAM face-auth policy -- the parts `pam` and `doctor` must agree on.
#
# Factored out of lib/pam.sh in slice 010 for the same reason slice 008 factored
# the dw9719 conditions into one implementation: doctor has to check exactly what
# `pam enable` writes, and two copies of a policy drift. lib/pam.sh runs a `case`
# dispatch when executed, so it cannot be sourced -- hence this file.
#
# Sourced by lib/pam.sh and lib/doctor.sh. Defines no state and runs nothing.

# Services face auth must NEVER be added to. This is the lockout surface, and
# the convenience is not worth it (slice 010's table). Enforced, not documented:
# the old pam-enable.sh would happily have edited any of these.
#   login / system-login / system-local-login / su  -- the console login path
#   system-auth   -- included by essentially every other service
#   other         -- the fallback stack for services with no file of their own
#   polkit-1      -- does not exist here; polkit currently falls through to
#                    'other'. Creating it is its own decision with its own blast
#                    radius, so it is refused rather than quietly created.
PAM_NEVER=(login system-login system-local-login su system-auth other polkit-1)

# Where the PAM service files live. CEMENT_IR_PAM_DIR overrides it so the stanza
# `pam enable` generates can be verified end to end without root and without
# touching the real auth path -- the same deliberate test hook as
# CEMENT_IR_FAULTS in slice 007. It is NOT a way around the root check: `pam
# enable` still demands root whenever PAM_DIR is the real /etc/pam.d.
PAM_DIR=${CEMENT_IR_PAM_DIR:-/etc/pam.d}

# workaround= is service-local and is NOT read from config.ini -- pam_howdy(8).
#
# `native` requires a usable terminal: PAM_TTY must name an open terminal whose
# foreground process group is the current one, with at least one matching std
# descriptor. pam_howdy(8) is explicit that "a graphical conversation may provide
# TTY metadata without providing matching standard-terminal descriptors, so
# native handling is unavailable in that case" -- and the journal already shows
# it disabling itself even under sudo ("Native prompt conversation unavailable,
# disabling prompt workaround"). Under a GTK locker it is inert by construction,
# so it is dropped rather than carried as a cargo-culted flag (slice 010 asked
# this question explicitly).
#
# `native-input` is the documented graphical alternative, and it is deliberately
# NOT used: its fallback injects one synthetic Enter through /dev/uinput at
# whatever currently holds keyboard focus -- "not targeted to the PAM prompt and
# may instead reach another application". pam_howdy(8) says to validate that per
# consumer. We have not, so we do not ship it.
pam_workaround_for() {   # pam_workaround_for <service> -> "workaround=native" | ""
    case ${1:?} in
        sudo) echo 'workaround=native' ;;   # TTY-backed
        *)    echo '' ;;                    # graphical / unknown: no workaround
    esac
}

# How pacman will treat a hand edit to this file. Three outcomes that matter:
#   backup        -- in the owning package's backup array: an upgrade leaves a
#                    .pacnew and our edit survives (the /etc/pam.d/sudo case)
#   revert:<pkg>  -- package-owned but NOT in any backup array: an upgrade
#                    OVERWRITES it silently, no .pacnew, no warning. Face auth
#                    just stops (the /etc/pam.d/gtklock case)
#   unowned       -- no package claims it; nothing will touch it
pam_pkg_state() {   # pam_pkg_state <file> -> backup | revert:<pkg> | unowned
    local f=${1:?} pkg
    command -v pacman >/dev/null 2>&1 || { echo unowned; return; }
    pkg=$(pacman -Qqo "$f" 2>/dev/null | awk '{print $NF}')
    [[ -n $pkg ]] || { echo unowned; return; }
    if pacman -Qii "$pkg" 2>/dev/null | sed -n '/^Backup Files/,/^[A-Z]/p' | grep -qF "$f"; then
        echo backup
    else
        echo "revert:$pkg"
    fi
}

# Every service currently carrying face auth, one per line. Machine-readable on
# purpose: doctor iterates this instead of hardcoding 'sudo' (slice 010 #4).
pam_enabled_services() {
    local f
    for f in "$PAM_DIR"/*; do
        [[ -f $f && -r $f ]] || continue
        case $f in *.pre-howdy|*.pacnew|*.pacsave|*.orig) continue ;; esac
        grep -q 'pam_howdy\.so' "$f" && echo "${f##*/}"
    done
    return 0
}
