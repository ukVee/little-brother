# What the August 2026 tree was, and where each of its findings went

The baseline commit of this repo is `~/cement-ir` exactly as the 21–23 August 2026
IR bring-up left it: 27 loose scripts plus a `__pycache__`, no version control, no
documentation outside their own comment headers. Several were single-purpose probes
whose *conclusion* mattered and whose *code* did not.

This file is the disposition record. It exists so that deleting a probe cannot
delete a finding, and so the restructure commit can be read as a diff rather than
taken on trust.

Conventions below: **garden** paths are relative to `~/soft-fig_garden`.

## Kept — the runtime path

| baseline file | now | why it survived |
|---|---|---|
| `irface.py` | `lib/irface.py` | the capture/unpack/feed engine and the torch owner. Gained a `torch` subcommand; otherwise unchanged. |
| `ip3y.py` | `lib/ip3y.py` | the `ip3y` unpacker, verified pixel-for-pixel against the reference Rust `ipu3-unpack`. |
| `tps68470.py` | `lib/tps68470.py` | the PMIC register surface + decoders. |
| `ir-up.sh` | `lib/ir-up.sh` | graph bring-up by entity name, gain/exposure applied per capture. |
| `sensor-recover.sh` | `lib/sensor-recover.sh` | the wedge fault handler (rebind `ov7251`, clear latched `GSTAT`). |
| `howdy-ir-pam` | `system/howdy-ir-pam.in` | the live `pam_exec` helper, now a template so the repo path is substituted at install time instead of hardcoded. |

## Folded — behaviour preserved under a subcommand

| baseline file | now | notes |
|---|---|---|
| `pam-enable.sh` | `cement-ir pam enable` | its safety header (root shell on another TTY; why neither PAM line can lock you out) is preserved verbatim in `lib/pam.sh`. |
| `pam-disable.sh` | `cement-ir pam disable` | restores `.pre-howdy`, else strips the lines. |
| `pam-fix.sh` | `cement-ir pam install-helper` | the `seteuid` repair is no longer needed as a separate step: `pam enable` writes the line with `seteuid`, and `install-helper` re-renders the helper. It also still creates the world-readable `/var/log/howdy-ir-pam.log`. |
| `set-device.sh` | `cement-ir pam set-device` | points howdy's `config.ini` at the loopback; keeps the one-time `config.ini.orig`. |
| `cement-ir.sh` | `cement-ir dkms install` / `verify` | both refusals kept: abort if `dkms` is absent, abort if the staged source has no `i2c_device_id` (cementing the broken driver is worse than doing nothing). The verification triple is now also a `doctor` check. |
| `loopback-persist.sh` | `cement-ir loopback install` | the two `/etc` files became data in `system/`, so the installed state can be diffed against the tree. |
| `howdy-ir.sh` | `cement-ir howdy <args>` | **it was in neither column of the milestone's consolidation table.** Decided here: folded, not deleted. It is the enrolment-time counterpart of the PAM helper and its line-68 torch-off-on-exit is behaviour `howdy-ir-pam` does not have — belt-and-braces over `irface.py`'s own cleanup, and the emitter is the one thing worth being paranoid about. |
| `capture-diag.sh` | `cement-ir diag capture` | escalating look → relink → module reload, output visible. Node resolved by name now, not `/dev/video2`. |
| `loopback-diag.sh` | `cement-ir diag loopback` | plus a third reader: OpenCV, the one that decides whether howdy works. |

## Deleted — findings recorded first

Each row states where the conclusion now lives. Nothing was deleted whose finding
was not already written down or written down as part of the same change.

| baseline file | what it proved | where the finding lives now |
|---|---|---|
| `ir-illum-test.sh` | First ILED sweep (2026-08-21): a clean negative — and a **false** one. It toggled `/sys/class/leds` brightness, which only flips an enable bit. | garden `projects/cement-ir/notes/001`; the false-negative mechanism in `hardware/notes/005`. |
| `ir-illum-16ma.sh` | Why that negative was false: `leds-tps68470` programs ILED B's current from a device property (`ti,ledb-current`) that `int3472` never supplies, so `CTRLB` stayed at its power-on default of **2 mA** — the whole sweep compared "off" against "on at the floor". Retested at 16 mA: still nothing. | garden `projects/cement-ir/notes/001`. |
| `ir-illum-hold.sh` | Eyeballing the bezel through a phone camera is more sensitive than measuring reflected light, but only for 850 nm parts; a 940 nm emitter is near-invisible to a phone with a decent IR-cut filter, so a visual negative is suggestive, not conclusive. | garden `projects/cement-ir/notes/001`. |
| `ir-emitter-probe.sh` | The presence test that did not need eyes: the PMIC flags its own load faults. `GSTAT[3] OVP` on the WLED boost **is** what an open (absent) load looks like; `GSTAT[5] ILEDF` + the read-only `ILEDCTL[3]/[7]` say open vs shorted for the indicator pins. Result: the ILED outputs fault, the WLED outputs do not — something is physically attached to WLED. | garden `projects/cement-ir/notes/001`. |
| `ir-wled.sh` | The WLED block is the flash/torch driver, a different output from the ILED indicators (16 mA cap): torch to 227.5 mA, flash to 1007.5 mA, and on this machine every WLED register still read its hardware reset value. Corroborated by linux-surface#739 (Surface Go 2, same path, 875 ms flash / 13 s torch timers). | `hardware/notes/005` (register map, the ILED-vs-WLED distinction); garden `projects/cement-ir/notes/001` (the #739 corroboration). |
| `ir-wled-capture.sh` | Measured the WLED torch **through the IR sensor**, interleaved and repeated with register readback — and the 130 mA × 40 s sustained hold that latched `PWR_FLAG\|OVP\|UVLO` and took the sensor down with it. | `hardware/notes/005` (the wedge, in full). |
| `ir-darkroom.sh` | The positive-control method: without a source known to be emitting, a null result from an emitter test is unfalsifiable. Also the conclusion that light reaching the sensor *through a hand* is IR-enriched and reads strongly ⇒ no tight IR bandpass filter is fitted, i.e. the sensor would comfortably see a working emitter. | garden `projects/cement-ir/notes/001`. |
| `iledctl.py` | Superseded by `tps68470.py`, which covers `ILEDCTL` (0x28) and decodes it. | `lib/tps68470.py`. |
| `detect-test.py` | Scored tuning frames by whether **dlib** finds a face, not by mean brightness — the operating point that counts is where the detector fires. Picked gain 128. | garden `projects/cement-ir/notes/001`. |
| `yunet-test.py` | howdy-next is a C++/OpenCV rewrite using **YuNet** + SFace, not dlib, with different exposure sensitivity — so the dlib operating point had to be re-measured rather than assumed. 32.5 mA (1 step) scored best; more current clips and scores *worse*. | garden `projects/cement-ir/notes/001`. |
| `feed-test.sh` | Proved the loopback path end-to-end before involving howdy, by pulling a frame back **out** of the loopback and running a detector on it. | garden `projects/cement-ir/notes/002`; the check itself is now `cement-ir diag loopback`'s third reader. |
| `ir-up.sh.bak` | The pre-renumbering variant: it derived the CIO2 node as `/dev/video$IDX` from the CSI-2 index. True on the boot it was written, false after a reboot (`csi2 2` paired with `/dev/video9`), and the mismatch makes `v4l2-ctl` block forever on a node with no producer, silently. | `lib/ir-up.sh`'s own comment; `hardware/notes/001`; garden `projects/cement-ir/notes/002`. |

## Two things the baseline referenced that do not exist

- `ip3y.py`'s docstring cited "garden note 006" for the packing layout and
  `validate_ip3y.py` for its verification. Neither existed: note numbering shifted
  (006 is now the front-RGB note), and the validation script was never saved. The
  citation is corrected to `projects/cement-ir/notes/002`; **re-validating the
  unpacker against `ipu3-unpack` would have to start from scratch.**
- `irface.py` cited "garden note 008" for the torch sequencing and "note 007" for
  the gain trap; both facts are in `hardware/notes/005`, and the references are
  corrected.

> Slice 006 (2026-09-26) superseded two rows above: `howdy-ir-pam` is now
> bombadil-sourced from `~/dotfiles/src/system/cement-ir/howdy-ir-pam` (a
> trigger client, no template, no repo path — `install-helper` was deleted
> with it), and the two `system/{modprobe,modules-load}.d/v4l2loopback.conf`
> data files moved to `~/dotfiles/src/system/` the same way. This file keeps
> the slice-003 record as it was.
