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
# Neither pass nor fail: the check could not mean anything right now, and
# saying why beats inventing a verdict.
note() { printf '  note  %-26s %s\n' "$1" "${2-}"; }
# warn: a real future failure that is not one yet. Does NOT set the exit code --
# a check that cries wolf gets ignored when it matters (the slice-006 lesson).
warn() { printf '  warn  %-26s %s\n' "$1" "${2-}"; }

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
            bad "runtime binary" "stale: $(basename "$newer") newer than $RT -- make -C $ROOT build && sudo make -C $ROOT install"
        else
            ok "runtime binary" "$RT up to date"
        fi
    else
        ok "runtime binary" "$RT (no crates/ in this checkout)"
    fi
else
    skip "runtime binary" "not installed -- make -C $ROOT build && sudo make -C $ROOT install"
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

# The watchdog around the carry (slice 008): the check that runs without being
# remembered. Both are bombadil-sourced and Exec the INSTALLED binary, so their
# absence is a deployment gap, not a code gap.
HOOK=/etc/pacman.d/hooks/90-cement-ir-dw9719.hook
if [[ -L $HOOK ]]; then
    ok "pacman hook" "bombadil-managed ($(readlink -f "$HOOK"))"
elif [[ -e $HOOK ]]; then
    bad "pacman hook" "present but not a bombadil symlink -- run: bombadil link"
else
    bad "pacman hook" "absent -- a kernel upgrade can break the carry unwatched (sudo mkdir -p ${HOOK%/*}; bombadil link)"
fi
if systemctl cat cement-ir-dkms-check.service >/dev/null 2>&1; then
    systemctl is-enabled cement-ir-dkms-check.service >/dev/null 2>&1 \
        && ok "boot check unit" "enabled" \
        || bad "boot check unit" "installed but not enabled -- sudo systemctl enable cement-ir-dkms-check.service"
else
    bad "boot check unit" "cement-ir-dkms-check.service not installed (bombadil link + daemon-reload)"
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
DOTS_HOME=$(real_home)
DOTS_SYSTEM="$DOTS_HOME/dotfiles/src/system"
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
        # The helper reaches the binary through $RT, so the literal string
        # "cement-ir-rt irfaced trigger" is never in it -- this grep FAILed a
        # correct bombadil deploy on 2026-09-26. Assert what is really there:
        # the installed path it resolves, and the trigger it issues.
        if [[ $real == "$DOTS_HOME"/dotfiles/.dots/* ]] \
           && grep -q '^RT=/usr/local/bin/cement-ir-rt$' /usr/local/bin/howdy-ir-pam \
           && grep -q 'irfaced trigger' /usr/local/bin/howdy-ir-pam; then
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
# Every service carrying face auth, not just sudo (slice 010 #4). The set is
# {sudo} U {whatever pam.sh reports}, so sudo going missing is still a FAIL
# rather than quietly dropping out of the loop.
source "$LIB/pam-policy.sh"
pam_services=$(printf '%s\n' sudo $(pam_enabled_services) | sort -u)
for svc in $pam_services; do
    f=$PAM_DIR/$svc
    if [[ ! -r $f ]]; then
        skip "$svc: pam lines" "$f not readable"
        continue
    fi
    grep -q 'pam_exec.so seteuid /usr/local/bin/howdy-ir-pam' "$f" \
        && ok "$svc: feeder line" "auth optional + seteuid" \
        || bad "$svc: feeder line" "missing or missing 'seteuid' (helper runs as uid 1000 and dies silently)"
    if grep -q 'auth.*sufficient.*pam_howdy.so' "$f"; then
        ok "$svc: pam_howdy line" "sufficient (failure falls through to the password)"
        # The howdy lines must sit ABOVE the include: Arch's system-auth ends in
        # [default=die] pam_faillock authfail, so a 'sufficient' below the
        # include is unreachable after a wrong password.
        h=$(grep -n 'pam_howdy\.so' "$f" | head -1 | cut -d: -f1)
        i=$(grep -nE '^auth[[:space:]]+include' "$f" | head -1 | cut -d: -f1)
        if [[ -n ${i:-} ]]; then
            [[ $h -lt $i ]] && ok "$svc: ordering" "howdy above 'auth include' (line $h < $i)" \
                || bad "$svc: ordering" "pam_howdy is BELOW 'auth include' (line $h > $i) -- unreachable after a failed password"
        fi
        # The workaround= option is service-local; policy lives in pam-policy.sh.
        want=$(pam_workaround_for "$svc")
        got=$(sed -n 's/.*pam_howdy\.so[[:space:]]*//p' "$f" | head -1)
        [[ ${got:-} == "${want:-}" ]] \
            && ok "$svc: workaround" "${got:-none (graphical: native is inert, native-input unvalidated)}" \
            || bad "$svc: workaround" "'${got:-none}' -- policy for $svc is '${want:-none}'"
    else
        bad "$svc: pam_howdy line" "absent"
    fi
    [[ -f $f.pre-howdy ]] && ok "$svc: backup" "$f.pre-howdy" \
        || bad "$svc: backup" "no .pre-howdy to restore from"
    # A package-owned file that is NOT in pacman's backup array gets overwritten
    # silently on upgrade -- no .pacnew, no warning, face auth just stops. That
    # is /etc/pam.d/gtklock. Warn while it is still enabled, so the revert is
    # visible here instead of being discovered weeks later (slice 010).
    case $(pam_pkg_state "$f") in
        revert:*) st=$(pam_pkg_state "$f")
                  warn "$svc: pacman revert risk" "owned by ${st#revert:} and not a backup file -- an upgrade silently restores the stock file; re-run 'pam enable $svc' after one" ;;
    esac
done
# The lockout surface must stay untouched (slice 010 #3). There are no
# .pre-howdy backups to diff against for these -- we never touched them -- so the
# invariant is stated positively: none of them carries a howdy line, in either
# /etc/pam.d or the vendor dir PAM falls back to. pam_workaround_for()'s sibling
# PAM_NEVER is the list, and `pam enable` refuses them, so this is the check that
# a hand edit outside the tool did not slip one in.
dirty=()
for n in "${PAM_NEVER[@]}"; do
    for d in "$PAM_DIR" /usr/lib/pam.d; do
        [[ -r $d/$n ]] || continue
        grep -qE 'pam_howdy|howdy-ir-pam' "$d/$n" && dirty+=("$d/$n")
    done
done
(( ${#dirty[@]} == 0 )) \
    && ok "never-touch services clean" "${#PAM_NEVER[@]} checked (login, su, system-auth, other, polkit-1, ...)" \
    || bad "never-touch services clean" "face auth found on the lockout surface: ${dirty[*]}"

pacnew=$(ls "$PAM_DIR"/*.pacnew 2>/dev/null | tr '\n' ' ')
[[ -z $pacnew ]] && ok "no .pacnew in pam.d" || bad "no .pacnew in pam.d" "$pacnew"
if [[ -r /etc/howdy/config.ini ]]; then
    dp=$(sed -n 's/^device_path = //p' /etc/howdy/config.ini)
    [[ $dp == /dev/video20 ]] && ok "howdy device_path" "$dp" \
        || bad "howdy device_path" "'${dp:-unset}' -- want /dev/video20 (sudo $ROOT/cement-ir pam set-device)"
    # The invoking human, not a hardcoded name -- and howdy's layout is not
    # assumed: count BOTH <user>.dat and <user>/*.dat. The hardcoded
    # `models/ukv/*.dat` reported "0 enrolled models" on a machine that had
    # just authenticated by face six times (2026-09-26).
    u=$(real_user)
    models=$( { ls "/etc/howdy/models/$u".dat; ls "/etc/howdy/models/$u"/*.dat; } 2>/dev/null | wc -l )
    [[ ${models:-0} -ge 1 ]] && ok "howdy model for $u" "$models" \
        || bad "howdy model for $u" "0 enrolled models under /etc/howdy/models -- sudo $ROOT/cement-ir howdy add"
    # config.ini now carries a deliberate local edit (dark_threshold 75 -> 80,
    # journal/decisions/decision-raise-howdy-dark-threshold.md), so an upstream
    # change to the stock defaults is something we need TOLD about. It is the
    # mirror image of the pam.d case checked above: config.ini IS in
    # howdy-next's backup array, so pacman leaves a .pacnew instead of
    # overwriting -- which only helps if something reads it. /etc/pam.d/gtklock
    # is not a backup file, so it gets silently restored and never leaves one.
    hp=$(ls /etc/howdy/*.pacnew 2>/dev/null | tr '\n' ' ')
    [[ -z $hp ]] && ok "no .pacnew in /etc/howdy" "the local dark_threshold edit is unchallenged" \
        || bad "no .pacnew in /etc/howdy" "$hp -- upstream defaults moved; diff it before merging, the local edit is deliberate"
else
    skip "howdy device_path" "/etc/howdy is 0750 root:root -- run doctor with sudo to read it"
    skip "no .pacnew in /etc/howdy" "/etc/howdy is 0750 root:root -- run doctor with sudo to read it"
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
            # Running doctor with sudo AUTHENTICATES, and that authentication
            # lights the emitter for the length of a feed -- so these two
            # checks were reading the PMIC mid-feed and failing on a machine
            # doing exactly what it should (2026-09-26: `GSTAT 0x48` +
            # `WLEDCTL 0x25 en=1 start=1`, both reported as problems, while
            # irfaced was serving this very sudo). Say so instead of lying.
            if systemctl is-active --quiet irfaced.service 2>/dev/null; then
                note "PMIC GSTAT" "a feed is in flight (irfaced active) -- this sudo's own auth lit the emitter; re-run once it has idled out"
                note "torch EN" "same: the emitter is lit for this feed, by design"
            else
                # A latched OVP|UVLO after a feed has ENDED is the real signal.
                # Note that a latch at release is normal on this hardware (20/20
                # cycles, D14) and the teardown clears it; one standing here
                # means no teardown ran -- an abnormal exit.
                grep -qE 'OVP|UVLO' <<<"$out" \
                    && bad "PMIC GSTAT" "latched with no feed running -- an abnormal exit left it; sudo $ROOT/cement-ir recover" \
                    || ok "PMIC GSTAT" "no OVP|UVLO"
                grep -q 'WLEDCTL.*en=1' <<<"$out" \
                    && bad "torch EN" "WLEDCTL EN still set with no feed running" \
                    || ok "torch EN" "clear"
            fi
        fi
    else
        skip "PMIC status" "$RT not installed -- make -C $ROOT build && sudo make -C $ROOT install"
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
