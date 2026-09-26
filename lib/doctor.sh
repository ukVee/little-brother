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
# guessing. Slice 004 of the milestone hardens this into the single source of
# truth for "is this healthy" (DKMS srcversion vs loaded, .pacnew detection,
# /etc/howdy device_path, exit-non-zero naming the first failure).
set -uo pipefail

source "$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)/common.sh"

FAILED=()
ok()   { printf '  ok    %-26s %s\n' "$1" "${2-}"; }
bad()  { printf '  FAIL  %-26s %s\n' "$1" "${2-}"; FAILED+=("$1: ${2-}"); }
skip() { printf '  skip  %-26s %s\n' "$1" "${2-}"; }

echo "=== repo ==="
ok "root" "$ROOT"

echo "=== dependencies ==="
for c in media-ctl v4l2-ctl ffmpeg python3; do
    if have "$c"; then ok "$c" "$(command -v "$c")"; else bad "$c" "not installed"; fi
done
have howdy && ok howdy "$(howdy --version 2>/dev/null | head -1)" || bad howdy "not installed"
python3 -c 'import numpy' 2>/dev/null && ok python-numpy || bad python-numpy "missing (irface.py cannot unpack frames)"
# The readiness gate the PAM helper uses. Slice 004 removes this dependency; until
# then its absence is the difference between face auth working and silently not.
python3 -c 'import cv2' 2>/dev/null && ok python-opencv \
    || bad python-opencv "missing -- the PAM readiness gate cannot pass (pacman -S python-opencv)"

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
[[ -e /sys/bus/i2c/devices/i2c-INT347A:00-VCM/driver ]] && ok "VCM bound" \
    || bad "VCM bound" "i2c-INT347A:00-VCM has no driver"

echo "=== media graph (resolved by entity name -- node numbers move) ==="
sensor=$(media_entity_sensor)
if [[ -n $sensor ]]; then
    ok "ov7251 entity" "$sensor"
    link=$(media-ctl -p -d /dev/media0 2>/dev/null \
           | sed -n "/entity [0-9]*: $sensor/,/^- entity/p" | grep -c '\->' || true)
    [[ ${link:-0} -ge 1 ]] && ok "sensor -> csi2 link" || bad "sensor -> csi2 link" "no link in the graph"
    sd=$(media_entity_subdev)
    [[ -n $sd ]] && ok "sensor subdev" "$sd" || bad "sensor subdev" "not resolvable"
else
    bad "ov7251 entity" "absent from /dev/media0 -- check the dw9719 carry above"
fi

echo "=== loopback ==="
if [[ -e /dev/video20 ]]; then
    ok "/dev/video20" "$(v4l2-ctl -d /dev/video20 --info 2>/dev/null | sed -n 's/\s*Card type\s*: //p')"
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
    ok "howdy device_path" "$(sed -n 's/^device_path = //p' /etc/howdy/config.ini)"
else
    skip "howdy device_path" "/etc/howdy is 0750 root:root -- run doctor with sudo to read it"
fi

echo "=== last auth attempt (the only record ukv can read) ==="
if [[ -r /var/log/howdy-ir-pam.log ]]; then
    tail -4 /var/log/howdy-ir-pam.log | sed 's/^/  /'
else
    skip "howdy-ir-pam.log" "not readable"
fi

echo "=== PMIC ==="
if [[ $EUID -eq 0 ]]; then
    python3 "$LIB/tps68470.py" status
    # A latched OVP|UVLO means the emitter browned out the sensor's rail; the
    # sensor will be mute on i2c until a rebind. cement-ir recover clears both.
else
    skip "tps68470 status" "needs root (/dev/i2c-2 is root-only)"
fi

echo
if (( ${#FAILED[@]} )); then
    echo "doctor: ${#FAILED[@]} problem(s); first: ${FAILED[0]}" >&2
    exit 1
fi
echo "doctor: all checks passed"
