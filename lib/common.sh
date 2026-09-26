# Shared helpers. Sourced by the cement-ir entry point and by any lib script
# that can also be run on its own.
#
# NOTHING IN THIS REPO MAY HARDCODE ITS OWN INSTALL PATH. The August tree wrote
# /home/ukv/cement-ir into 16 files, so one plain `mv` of the directory broke
# face auth silently -- the installed PAM helper kept pointing at a path that no
# longer existed, and because it is `auth optional` and always exits 0, sudo
# simply stopped trying the camera. Every path below is derived from where this
# file actually is, so moving the repo again costs nothing. (Since slice 006 the
# PAM helper is bombadil-sourced and carries no repo path at all; nothing
# outside this tree hardcodes it.)

ROOT=$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")/.." && pwd)
LIB="$ROOT/lib"
SYSTEM="$ROOT/system"

die() { echo "cement-ir: $*" >&2; exit 1; }

need_root() {   # need_root <subcommand>
    [[ $EUID -eq 0 ]] || die "'${1:-this}' needs root:  sudo $ROOT/cement-ir ${1:-...} ..."
}

have() { command -v "$1" >/dev/null 2>&1; }

# The emitter's one owner (slice 005): the Rust runtime binary. Installed
# wins (the /usr/local/bin discipline); before `make install` has run, the
# in-tree release build stands in so the path keeps working. Every torch
# call in every script goes through this -- nothing else writes a PMIC
# register.
rt_binary() {
    if have cement-ir-rt; then
        command -v cement-ir-rt
        return
    fi
    local cand="$ROOT/crates/cement-ir/target/release/cement-ir-rt"
    [[ -x $cand ]] && echo "$cand"
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
