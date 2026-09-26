#!/usr/bin/env bash
# cement-ir doctor -- one read-only pass over everything the IR face-auth path
# depends on, one line per check, first failure named at the end.
#
# The point of this command: an orphan sweep that deletes a dependency should be
# caught here, not by a failed login three weeks later. That is exactly what
# happened on 2026-09-06 (python-opencv swept by `pacman -Rns`, readiness gate
# dead ever since, 23 consecutive silent failures) -- see
# journal/incidents/incident-20260926-debloat-orphan-sweep-broke-howdy-readiness.md
#
# Read-only by design: it never rebinds a driver, writes a PMIC register, or
# touches /etc. Some checks need root to see anything and say so rather than
# guessing. Slice 004 hardened this into the single source of truth for "is
# this healthy" -- the check table is the slice's spec, first failure named,
# exit non-zero.
#
# --quick: the cheap subset the PAM helper runs in front of the feeder
# (dw9719 module, sensor->csi2 link, loopback) -- one verdict line, nothing
# that needs root or takes real time. The expensive checks stay manual.
set -uo pipefail

QUICK=0
[[ ${1:-} == --quick ]] && { QUICK=1; shift; }

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"

FAILED=()
ok()   { printf '  ok    %-26s %s\n' "$1" "${2-}"; }
bad()  { printf '  FAIL  %-26s %s\n' "$1" "${2-}"; FAILED+=("$1: ${2-}"); }
skip() { printf '  skip  %-26s %s\n' "$1" "${2-}"; }

echo "=== repo ==="
ok "root" "$ROOT"

if (( ! QUICK )); then
echo "=== dependencies ==="
for c in media-ctl v4l2-ctl ffmpeg python3; do
    if have "$c"; then ok "$c" "$(command -v "$c")"; else bad "$c" "not installed"; fi
done
have howdy && ok howdy "$(howdy --version 2>/dev/null | head -1)" || bad howdy "not installed"
python3 -c 'import numpy' 2>/dev/null && ok python-numpy || bad python-numpy "missing (irface.py cannot unpack frames)"
# python-opencv is deliberately NOT here: the readiness gate used to import it
# and the 2026-09-06 orphan sweep deleted it; since slice 004 the feeder
# signals its own readiness, so nothing on the auth path imports cv2 and
# nothing should reinstall it out of caution.
fi

if (( ! QUICK )); then
echo "=== runtime core (Rust, slice 013) ==="
# Installed, never run out of target/ (decision-cement-ir-rust-runtime-core):
# a `cargo clean` must not be able to break sudo. Nothing on the auth path
# depends on it yet (slice 004 cuts over), so absent = skip; STALE = bad,
# same logic as the PAM helper check below.
RT=/usr/local/bin/cement-ir-rt
if [[ -x $RT ]]; then
    if [[ -d $ROOT/crates ]]; then
        newer=$(find "$ROOT/crates" -name '*.rs' -newer "$RT" -print -quit 2>/dev/null)
        if [[ -n $newer ]]; then
            bad "runtime binary" "stale: $(basename "$newer") newer than $RT -- sudo make -C $ROOT install"
        else
            ok "runtime binary" "$RT up to date"
        fi
    else
        ok "runtime binary" "$RT (no crates/ in this checkout)"
    fi
else
    skip "runtime binary" "not installed -- sudo make -C $ROOT install"
fi
fi

echo "=== dw9719 carry (without it NO camera appears) ==="
# ONE implementation of these checks, in the runtime core (slice 008): the same
# code runs from the pacman hook on a linux-surface transaction and from the
# boot unit, so doctor cannot drift from what the hook enforces. --porcelain
# gives `status|name|detail`; we render it in doctor's own table.
RTV=$(rt_binary) || true
if [[ -n ${RTV:-} ]]; then
    while IFS='|' read -r st nm dt; do
        [[ -z ${st:-} ]] && continue
        if [[ $st == ok ]]; then ok "$nm" "$dt"; else bad "$nm" "$dt"; fi
    done < <("$RTV" dkms verify --porcelain 2>/dev/null)
else
    skip "dw9719 carry" "cement-ir-rt not built or installed -- make -C $ROOT build (then sudo make install)"
fi

echo "=== media graph (resolved by entity name -- node numbers move) ==="
sensor=$(media_entity_sensor)
if [[ -n $sensor ]]; then
    ok "ov7251 entity" "$sensor"
    sect=$(media-ctl -p -d /dev/media0 2>/dev/null \
           | sed -n "/entity [0-9]*: $sensor/,/^- entity/p" || true)
    links=$(grep -c '\->' <<<"$sect" || true)
    [[ ${links:-0} -eq 1 ]] && ok "sensor -> csi2 link" "1" \
        || bad "sensor -> csi2 link" "$links (want exactly 1)"
    grep -q 'fmt:Y10_1X10/640x480' <<<"$sect" \
        && ok "sensor format" "Y10_1X10/640x480" \
        || bad "sensor format" "not Y10_1X10/640x480 in the graph (run: sudo $ROOT/cement-ir up)"
    sd=$(media_entity_subdev)
    [[ -n $sd ]] && ok "sensor subdev" "$sd" || bad "sensor subdev" "not resolvable"
else
    bad "ov7251 entity" "absent from /dev/media0 -- check the dw9719 carry above"
fi

echo "=== loopback ==="
if [[ -e /dev/video20 ]]; then
    card=$(v4l2-ctl -d /dev/video20 --info 2>/dev/null | sed -n 's/\s*Card type\s*: //p')
    if [[ $card == *"Howdy IR"* ]]; then
        ok "/dev/video20" "$card"
    else
        bad "/dev/video20" "card label '${card:-none}' -- want 'Howdy IR' (modprobe.d option drift; sudo $ROOT/cement-ir loopback install)"
    fi
else
    bad "/dev/video20" "absent -- sudo $ROOT/cement-ir loopback install"
fi
# Compare the EFFECTIVE lines only -- a reworded comment is not a drift, and a
# check that cries wolf over one gets ignored when it matters. The source of
# truth moved to ~/dotfiles with slice 006 (bombadil-sourced).
effective() { grep -vE '^\s*(#|$)' "$1" 2>/dev/null; }
DOTS_SYSTEM="$HOME/dotfiles/src/system"
for f in /etc/modprobe.d/v4l2loopback.conf /etc/modules-load.d/v4l2loopback.conf; do
    if [[ -f $f ]]; then
        if diff -q <(effective "$f") <(effective "$DOTS_SYSTEM/${f#/etc/}") >/dev/null; then
            ok "${f#/etc/}" "matches ~/dotfiles"
        else
            bad "${f#/etc/}" "differs from $DOTS_SYSTEM/${f#/etc/} -- bombadil link"
        fi
    else
        bad "${f#/etc/}" "not installed -- bombadil link"
    fi
done

if (( ! QUICK )); then
echo "=== PAM ==="
# The helper is bombadil-sourced since slice 006: /usr/local/bin/howdy-ir-pam
# is a symlink into ~/dotfiles/.dots/ carrying the trigger call. There is no
# baked-in repo path anymore, so there is nothing to go stale -- but the
# pre-slice-006 copies (hand-installed, hardcoded path) are still out there
# and must be caught until D2 deploys the new helper.
if [[ -e /usr/local/bin/howdy-ir-pam ]]; then
    if [[ -L /usr/local/bin/howdy-ir-pam ]]; then
        real=$(readlink -f /usr/local/bin/howdy-ir-pam)
        if [[ $real == "$HOME"/dotfiles/.dots/* ]] && grep -q 'cement-ir-rt irfaced trigger' /usr/local/bin/howdy-ir-pam; then
            ok "installed helper" "bombadil-managed ($real)"
            # Slice 007: the rebind is a fault handler, never a pre-auth step.
            # This is the grep that keeps it that way.
            grep -qE 'sensor-recover|/(un)?bind' /usr/local/bin/howdy-ir-pam \
                && bad "helper has no rebind" "the installed helper still recovers on the happy path" \
                || ok "helper has no rebind" "no recovery on the pre-auth path"
        else
            bad "installed helper" "not the bombadil helper ($real) -- run: bombadil link"
        fi
    else
        bad "installed helper" "hand-installed copy, not a bombadil symlink -- run: bombadil link"
    fi
else
    bad "installed helper" "/usr/local/bin/howdy-ir-pam missing (face auth cannot trigger the daemon)"
fi
if [[ -r /etc/pam.d/sudo ]]; then
    grep -q 'pam_exec.so seteuid /usr/local/bin/howdy-ir-pam' /etc/pam.d/sudo \
        && ok "sudo: feeder line" "auth optional + seteuid" \
        || bad "sudo: feeder line" "missing or missing 'seteuid' (helper runs as uid 1000 and dies silently)"
    grep -q 'auth.*sufficient.*pam_howdy.so' /etc/pam.d/sudo \
        && ok "sudo: pam_howdy line" "sufficient (failure falls through to the password)" \
        || bad "sudo: pam_howdy line" "absent"
    [[ -f /etc/pam.d/sudo.pre-howdy ]] && ok "sudo: backup" "/etc/pam.d/sudo.pre-howdy" \
        || bad "sudo: backup" "no .pre-howdy to restore from"
else
    skip "sudo: pam lines" "/etc/pam.d/sudo not readable"
fi
pacnew=$(ls /etc/pam.d/*.pacnew 2>/dev/null | tr '\n' ' ')
[[ -z $pacnew ]] && ok "no .pacnew in pam.d" || bad "no .pacnew in pam.d" "$pacnew"
if [[ -r /etc/howdy/config.ini ]]; then
    dp=$(sed -n 's/^device_path = //p' /etc/howdy/config.ini)
    [[ $dp == /dev/video20 ]] && ok "howdy device_path" "$dp" \
        || bad "howdy device_path" "'${dp:-unset}' -- want /dev/video20 (sudo $ROOT/cement-ir pam set-device)"
    models=$(ls /etc/howdy/models/ukv/*.dat 2>/dev/null | wc -l)
    [[ ${models:-0} -ge 1 ]] && ok "howdy model for ukv" "$models" \
        || bad "howdy model for ukv" "0 enrolled models -- sudo $ROOT/cement-ir howdy add"
else
    skip "howdy device_path" "/etc/howdy is 0750 root:root -- run doctor with sudo to read it"
fi
fi

if (( ! QUICK )); then
echo "=== last auth attempt (the only record ukv can read) ==="
if [[ -r /var/log/howdy-ir-pam.log ]]; then
    tail -4 /var/log/howdy-ir-pam.log | sed 's/^/  /'
else
    skip "howdy-ir-pam.log" "not readable"
fi
fi

if (( ! QUICK )); then
echo "=== fault rate (since boot) ==="
# Slice 007: the ov7251 rebind is a fault handler now, not a routine step, so
# how often it fires is the number that matters. The counters live in tmpfs
# ($FAULTS), are bumped by sensor-recover.sh (recover) and cement-ir-rt
# (wedge), and reset at boot -- so a non-zero count is a fault THIS boot.
rec=$(fault_count recover); wed=$(fault_count wedge)
if [[ ! -e $FAULTS ]]; then
    ok "wedges since boot" "0 (no fault has been recorded)"
    ok "recoveries since boot" "0 (the rebind has not fired)"
else
    [[ $wed -eq 0 ]] && ok "wedges since boot" "0" \
        || bad "wedges since boot" "$wed -- journalctl -t cement-ir -u irfaced (grep FAULT)"
    [[ $rec -eq 0 ]] && ok "recoveries since boot" "0 (the rebind has not fired)" \
        || bad "recoveries since boot" "$rec rebind(s) -- the wedge is back; see $FAULTS"
fi
# The demotion itself, machine-checked: nothing the auth path runs may CALL
# the handler. Comment lines are excluded on purpose -- the files explain the
# demotion, and a check that fails on its own documentation is noise.
# (`cement-ir soak` and `cement-ir recover` do call it; they are not the path.)
callers=()
for p in "$LIB/pam.sh" "$LIB/howdy.sh" "$LIB/irface.py"; do
    [[ -r $p ]] || continue
    grep -vE '^[[:space:]]*#' "$p" | grep -q 'sensor-recover' && callers+=("$(basename "$p")")
done
if (( ${#callers[@]} )); then
    bad "no rebind on the auth path" "${callers[*]} still call sensor-recover.sh"
else
    ok "no rebind on the auth path" "pam.sh / howdy.sh / irface.py are clean"
fi
fi

if (( ! QUICK )); then
echo "=== PMIC ==="
if [[ $EUID -eq 0 ]]; then
    if [[ -x $RT ]]; then
        out=$("$RT" torch status 2>&1)
        rc=$?
        echo "$out" | sed 's/^/  /'
        if [[ $rc -ne 0 ]]; then
            bad "PMIC status" "cement-ir-rt torch status exited $rc"
        else
            # A latched OVP|UVLO means the emitter browned out the sensor's rail;
            # the sensor will be mute on i2c until a rebind (cement-ir recover).
            grep -qE 'OVP|UVLO' <<<"$out" \
                && bad "PMIC GSTAT" "latched -- sudo $ROOT/cement-ir recover" \
                || ok "PMIC GSTAT" "no OVP|UVLO"
            grep -q 'WLEDCTL.*en=1' <<<"$out" \
                && bad "torch EN" "WLEDCTL EN still set" \
                || ok "torch EN" "clear"
        fi
    else
        skip "PMIC status" "$RT not installed -- sudo make -C $ROOT install"
    fi
else
    skip "PMIC status" "needs root (/dev/i2c-2 is root-only)"
fi
fi

echo
if (( ${#FAILED[@]} )); then
    echo "doctor: ${#FAILED[@]} problem(s); first: ${FAILED[0]}" >&2
    exit 1
fi
echo "doctor: all checks passed"
