# Shared helpers. Sourced by the cement-ir entry point and by any lib script
# that can also be run on its own.
#
# NOTHING IN THIS REPO MAY HARDCODE ITS OWN INSTALL PATH. The August tree wrote
# /home/ukv/cement-ir into 16 files, so one plain `mv` of the directory broke
# face auth silently -- the installed PAM helper kept pointing at a path that no
# longer existed, and because it is `auth optional` and always exits 0, sudo
# simply stopped trying the camera. Every path below is derived from where this
# file actually is, so moving the repo again costs nothing. (Since slice 006 the
# PAM helper comes from ~/dotfiles and carries no repo path at all; nothing
# outside this tree hardcodes it.)

ROOT=$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")/.." && pwd)
LIB="$ROOT/lib"
SYSTEM="$ROOT/system"

die() { echo "cement-ir: $*" >&2; exit 1; }

# The human this machine belongs to, and their home -- NOT $USER/$HOME, which
# under `sudo` are root and /root. Half of `cement-ir doctor`'s checks compare
# against ~/dotfiles, so reading $HOME made `sudo doctor` look for
# /root/dotfiles and report three healthy things as broken (2026-09-26). A
# health tool whose answer depends on how it was invoked is worse than no
# health tool, so every dotfiles-relative path goes through these.
real_user() {
    printf '%s\n' "${SUDO_USER:-${USER:-$(id -un)}}"
}

real_home() {
    local u; u=$(real_user)
    local h; h=$(getent passwd "$u" 2>/dev/null | cut -d: -f6)
    printf '%s\n' "${h:-$HOME}"
}

# System files (the units, the pam_exec helper, the pacman hook, the
# loopback confs) are root-owned COPIES of ~/dotfiles/src/system/, installed
# by `sudo dotfiles-deploy-system` from its deploy.manifest. Until 2026-10-03
# they were bombadil symlinks into ~/dotfiles, which let any ukv process edit
# what root runs: the helper runs as root on every sudo and gtklock unlock
# (garden: services/notes/001, projects/facial-id-defender P0a). So a symlink,
# a non-root owner, a group/other-writable mode or an ACL is a FAIL even when
# the content is right. Content compares effective lines only: a reworded
# comment is not drift, and a check that cries wolf over one gets ignored.
deployed_state() {   # deployed_state <target> <source relative to src/system/>
    local t=$1 rel=$2 s
    s="$(real_home)/dotfiles/src/system/$2"
    if [[ -L $t ]]; then
        echo "a symlink -> $(readlink "$t") (root would run a file ukv can edit) -- sudo dotfiles-deploy-system"
        return 1
    fi
    [[ -e $t ]] || { echo "not installed -- sudo dotfiles-deploy-system"; return 1; }
    [[ $(stat -c %U "$t") == root ]] \
        || { echo "owned by $(stat -c %U "$t"), not root -- sudo dotfiles-deploy-system"; return 1; }
    [[ -z $(find "$t" -perm /022 2>/dev/null) ]] \
        || { echo "group/other-writable ($(stat -c %A "$t")) -- sudo dotfiles-deploy-system"; return 1; }
    [[ -z $(getfacl -s -p "$t" 2>/dev/null) ]] \
        || { echo "carries an ACL -- sudo dotfiles-deploy-system"; return 1; }
    if [[ ! -r $s ]]; then
        echo "root-owned copy, but ~/dotfiles/src/system/$rel is missing"; return 1
    fi
    diff -q <(grep -vE '^\s*(#|$)' "$t" 2>/dev/null) <(grep -vE '^\s*(#|$)' "$s") >/dev/null \
        || { echo "differs from ~/dotfiles/src/system/$rel -- sudo dotfiles-deploy-system"; return 1; }
    echo "root-owned copy, matches ~/dotfiles/src/system/$rel"
}

need_root() {   # need_root <subcommand>
    [[ $EUID -eq 0 ]] || die "'${1:-this}' needs root:  sudo $ROOT/cement-ir ${1:-...} ..."
}

have() { command -v "$1" >/dev/null 2>&1; }

# The emitter's one owner (slice 005): the Rust runtime binary. Installed
# wins (the /usr/local/bin discipline); before `make install` has run, the
# in-tree release build stands in so the path keeps working. Every torch
# call in every script goes through this -- nothing else writes a PMIC
# register.
# The workspace target dir is crates/target/, NOT crates/<crate>/target/ --
# the old path here never existed, so before `make install` every caller
# silently got "no torch owner": sensor-recover.sh could not read the PMIC and
# howdy.sh's belt-and-braces `torch off` on exit quietly did nothing. Found
# and fixed in slice 007.
rt_binary() {
    if have cement-ir-rt; then
        command -v cement-ir-rt
        return
    fi
    local cand="$ROOT/crates/target/release/cement-ir-rt"
    [[ -x $cand ]] && echo "$cand"
}

# The fault counters (slice 007): tmpfs, since boot, world-readable,
# root-written. Bumped by sensor-recover.sh (recover) and by cement-ir-rt
# (wedge); reported by doctor. See crates/cement-ir/src/faults.rs.
# CEMENT_IR_FAULTS overrides it in both halves (crates/cement-ir/src/faults.rs
# reads the same variable) so the shell-writes/Rust-reads round trip is testable.
FAULTS=${CEMENT_IR_FAULTS:-/run/cement-ir-faults}

fault_count() {   # fault_count <key> -> N (0 when the file or key is absent)
    local n
    n=$(awk -v k="${1:?}" '$1 == k { print $2 }' "$FAULTS" 2>/dev/null | tail -1)
    echo "${n:-0}"
}

# Bump a counter. Root-only (/run), atomic through a temp file + mv, and loud:
# a fault is an event now, not a silent step.
fault_bump() {    # fault_bump <key> <reason>
    local key=${1:?} reason=${2-} n
    n=$(( $(fault_count "$key") + 1 ))
    local tmp="$FAULTS.$$"
    { awk -v k="$key" '$1 != k { print }' "$FAULTS" 2>/dev/null; echo "$key $n"; } > "$tmp" \
        && mv -f "$tmp" "$FAULTS" && chmod 644 "$FAULTS" \
        || { rm -f "$tmp"; echo "cement-ir: WARNING: cannot record the $key fault in $FAULTS" >&2; }
    echo "cement-ir: FAULT $key #$n since boot: $reason" >&2
    have logger && logger -t cement-ir "FAULT $key #$n since boot: $reason"
    return 0
}

# The media graph renumbers across boots, so everything is resolved by entity
# name. Callers that hardcode a node number get a device with no producer and
# block forever with no error at all.
media_entity_sensor() {   # -> "ov7251 3-0060"
    media-ctl -p -d "${MEDIA:-/dev/media0}" 2>/dev/null \
        | sed -n 's/^- entity [0-9]*: \(ov7251 [0-9-]*\).*/\1/p' | head -1
}

media_entity_subdev() {   # -> /dev/v4l-subdevN for the ov7251
    media-ctl -p -d "${MEDIA:-/dev/media0}" 2>/dev/null \
        | sed -n '/entity [0-9]*: ov7251/,/^- entity/p' \
        | sed -n 's#.*device node name \(/dev/v4l-subdev[0-9]*\).*#\1#p' | head -1
}
