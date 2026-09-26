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
loaded=$(cat /sys/module/dw9719/srcversion 2>/dev/null || true)
dkmsv=$(modinfo -F srcversion dw9719 2>/dev/null || true)
alias_n=$(modinfo dw9719 2>/dev/null | grep -c '^alias:.*i2c:dw9719' || true)
[[ -n $loaded ]] && ok "module loaded" "srcversion $loaded" || bad "module loaded" "dw9719 is not loaded"
if [[ -n $loaded && -n $dkmsv ]]; then
    [[ $loaded == "$dkmsv" ]] && ok "srcversion matches" "$dkmsv" \
        || bad "srcversion matches" "loaded $loaded != on-disk $dkmsv (stock module squatted the name)"
fi
[[ ${alias_n:-0} -ge 1 ]] && ok "i2c alias present" "$alias_n" \
    || bad "i2c alias present" "0 -- this is the UNPATCHED module; no sensor will get a pad link"
# The bind the media-link creation depends on: the driver symlink must resolve
# to dw9719 itself, not merely exist (a bind to the stock module would squat
# the name the same way and still kill every camera).
vcm_drv=$(readlink /sys/bus/i2c/devices/i2c-INT347A:00-VCM/driver 2>/dev/null || true)
if [[ ${vcm_drv##*/} == dw9719 ]]; then
    ok "VCM bound" "i2c-INT347A:00-VCM -> dw9719"
elif [[ -n $vcm_drv ]]; then
    bad "VCM bound" "bound to ${vcm_drv##*/}, not dw9719"
else
    bad "VCM bound" "i2c-INT347A:00-VCM has no driver"
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
# check that cries wolf over one gets ignored when it matters.
effective() { grep -vE '^\s*(#|$)' "$1" 2>/dev/null; }
for f in /etc/modprobe.d/v4l2loopback.conf /etc/modules-load.d/v4l2loopback.conf; do
    if [[ -f $f ]]; then
        if diff -q <(effective "$f") <(effective "$SYSTEM/${f#/etc/}") >/dev/null; then
            ok "${f#/etc/}" "matches the repo"
        else
            bad "${f#/etc/}" "differs from $SYSTEM/${f#/etc/} -- sudo $ROOT/cement-ir loopback install"
        fi
    else
        bad "${f#/etc/}" "not installed"
    fi
done

if (( ! QUICK )); then
echo "=== PAM ==="
if [[ -f /usr/local/bin/howdy-ir-pam ]]; then
    hr=$(sed -n 's#^CEMENT_IR_ROOT=\(.*\)$#\1#p' /usr/local/bin/howdy-ir-pam | head -1)
    if [[ -z $hr ]]; then
        bad "installed helper" "pre-move copy with a hardcoded path -- sudo $ROOT/cement-ir pam install-helper"
    elif [[ $hr == "$ROOT" ]]; then
        ok "installed helper" "points at $hr"
    else
        bad "installed helper" "points at $hr, this checkout is $ROOT -- sudo $ROOT/cement-ir pam install-helper"
    fi
else
    bad "installed helper" "/usr/local/bin/howdy-ir-pam missing (face auth cannot start the feeder)"
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
