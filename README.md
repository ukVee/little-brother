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
- **`system/howdy-ir-pam.in`** is the `pam_exec` helper that starts the feeder
  just before `pam_howdy.so` runs. It is rendered and installed by
  `cement-ir pam install-helper`.

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
silently. The single unavoidable exception is the *installed copy* of the PAM
helper at `/usr/local/bin/howdy-ir-pam`, which lives outside the tree: it gets the
repo path substituted in at install time, and `cement-ir doctor` fails loudly when
that path no longer matches this checkout.

## Status

Work in progress under the garden milestone `ir-face-root-cause`. There is **no
git remote** — that is a deliberate, recorded decision, and the backup call is the
human's.

Not done yet, and known: the wedge recovery still runs on every auth (slice 007),
and `cement-ir-rt` is not yet installed to `/usr/local/bin` (deferred check D4 —
until it runs, the torch falls back to the in-tree build).
