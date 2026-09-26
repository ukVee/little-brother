# cement-ir

The IR camera path on one machine: an **archtablito** Surface Go 3 running Arch +
linux-surface, where the front IR sensor (`ov7251`) is used for face
authentication through howdy-next.

```
sudo ./cement-ir doctor            # is any of this healthy?
sudo ./cement-ir howdy add         # enrol a face model
./cement-ir --help                 # everything else
```

## What this is

One entry point (`cement-ir`) over a path the kernel does not assemble for us:

- **`lib/ir-up.sh`** brings the media graph up on demand, immediately before a
  capture, resolving every entity **by name** because the graph renumbers across
  boots.
- **`lib/irface.py`** captures raw `ip3y` frames from the CIO2 node, unpacks them
  (`lib/ip3y.py`), and republishes 8-bit greyscale on a v4l2loopback node that
  OpenCV — and therefore howdy — can actually read. It lights the illuminator
  *through* the owner (below), never directly.
- **`crates/`** — the Rust runtime core. `cement-ir-rt` (with `crates/tps68470`)
  is the emitter's **one owner**: the only code that writes a PMIC register, and
  it enforces a hard current ceiling (3 steps / 97.5 mA), a hard duration
  ceiling (120 s) and a GSTAT pre-check, releasing on every exit path
  (Drop + signals + `PR_SET_PDEATHSIG` + the ~13 s hardware watchdog).
- **`lib/tps68470.py`** *reads* the PMIC that powers the sensor *and* drives the
  emitter over raw i2c — read-only since slice 005, because nothing in Linux
  programs its WLED block and the owner is the only writer.
- **The PAM helper** (`/usr/local/bin/howdy-ir-pam`) is the `pam_exec` trigger
  that asks the on-demand face-capture daemon for a frame stream before
  `pam_howdy.so` runs. Bombadil-sourced since slice 006 from
  `~/dotfiles/src/system/cement-ir/howdy-ir-pam`; the daemon it triggers is
  `cement-ir-rt irfaced` (systemd units `irfaced.socket` + `irfaced.service`,
  also in `~/dotfiles/src/system/systemd/irfaced/`). Design:
  `docs/irfaced-design.md`.
- **`lib/sensor-recover.sh`** is the **fault handler**, not a step. It unbinds
  and rebinds `ov7251` to re-run power-up when the sensor has actually wedged
  (`cement-ir recover`). Until slice 007 it ran in front of *every*
  authentication attempt with a 25 s timeout — a recovery routine on the happy
  path, which turns a defect into latency and hides how often it fires. It now
  runs only after a failure, every rebind is counted in `/run/cement-ir-faults`
  (tmpfs, since boot), and `cement-ir doctor` reports the rate.
- **`cement-ir soak N`** is the evidence for that demotion: N full capture
  cycles back to back at the production operating point, through the same code
  the daemon runs, checking after each one that the sensor still answers I2C,
  that `GSTAT` has no new `OVP|UVLO` latch, and that the torch really went dark.
  One summary line; non-zero exit if any cycle wedged or needed a recovery.

## What this is not

- **Not a driver, and not a fix for one.** The `dw9719` DKMS carry
  (`cement-ir dkms`) is a local stopgap for a module that ships without its I2C
  `id_table`; the raw-CIO2 + userspace-unpack path exists because libcamera's
  IPU3 pipeline handler rejects this sensor outright. Both are permanent here by
  decision — this device is on-device-only: no upstream reports, no kernel or
  libcamera patches.
- **Not portable.** Entity names, ACPI ids, the PMIC address, the gain/exposure
  operating point and `/dev/video20` are all this machine's.
- **Not the documentation.** The durable knowledge lives in the garden
  (`~/soft-fig_garden`), which is the source of truth for *why*:
  - `hardware/notes/001` — the media graph and what actually works
  - `hardware/notes/005` — the TPS68470 topology, the wedge, the two traps
  - `hardware/notes/006` — why the front RGB sensor is absent (unrelated)
  - `os/kernel/notes/003` — the `dw9719` DKMS carry
  - `users/notes/001` — the PAM posture and how to reverse it
  - `projects/cement-ir/` — commentary on this repo, plus the notes this code
    is the only other record of
  - `docs/archaeology.md` (here) — what the August scripts were and where each
    of their findings went

## Layout

```
cement-ir          the only thing you run
crates/            the Rust runtime core (workspace; build/test/install via `make`)
lib/               internals; every path derived from the entry point's location
system/            files that get installed into the system
docs/              repo-local records that are not garden material
```

**No file in this repo hardcodes its own install path.** The August tree wrote
`/home/ukv/cement-ir` into 16 files, so one `mv` of the directory broke face auth
silently. Since slice 006 there is no exception left: the installed PAM helper
carries no repo path at all (it is a bombadil symlink whose one job is to
trigger the daemon over a socket), and `cement-ir doctor` checks that the
installed copy really is that symlink. Where a path *must* cross the boundary
into the Rust core — the fault handler `cement-ir soak` calls — the shell entry
point passes it in (`--recover-cmd`), so the binary knows no repo path either.

## Status

Work in progress under the garden milestone `ir-face-root-cause`. There is **no
git remote** — that is a deliberate, recorded decision, and the backup call is the
human's.

Not done yet, and known: `cement-ir-rt` is not yet installed to `/usr/local/bin`
(deferred check D4 — until it runs, the torch falls back to the in-tree release
build), and the socket deployment waits on it. The wedge recovery came off the
auth path in slices 006/007; the soak that proves it is `cement-ir soak 20`,
whose run **with the illuminator** needs root and is a deferred check.
