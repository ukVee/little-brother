# The `dw9719` carry — the patched source, under version control

> Last reviewed: 2026-10-02

This directory holds the **build input** for the DKMS package `dw9719-fix/1.0`.
`cement-ir dkms install` stages `dw9719.c` from here into `/usr/src/dw9719-fix-1.0/`,
generates the `Makefile` + `dkms.conf` beside it, and builds. Nothing else in the
repo produces a kernel module.

It lives here, under git, because losing it means re-deriving the patch. Until
2026-09-26 it lived only at `~/dw9719fix/dw9719.c` — outside version control,
outside the repo, one `rm -rf` from being gone while the DKMS registration that
depends on it stayed happily installed. (`lib/dkms.sh` still honours a `STAGED=`
override, so the old location can be pointed at explicitly if it is ever needed.)

## What the patch is

Two deltas against upstream `drivers/media/i2c/dw9719.c`.

### 1. The I2C device id table (2026-08)

The I2C device id table that linux-surface `6.19.8.arch1-3` ships
without, plus its `MODULE_DEVICE_TABLE`. Upstream fix
**`d7fe0d53b2a8`** — *"media: dw9719: Add back the I2C device id table"*,
stable-tagged for v6.19 and later:

```c
static const struct i2c_device_id dw9719_id_table[] = {
	{ .name = "dw9718s", .driver_data = (kernel_ulong_t)DW9718S },
	{ .name = "dw9719",  .driver_data = (kernel_ulong_t)DW9719  },
	{ .name = "dw9761",  .driver_data = (kernel_ulong_t)DW9761  },
	{ .name = "dw9800k", .driver_data = (kernel_ulong_t)DW9800K },
	{ }
};
MODULE_DEVICE_TABLE(i2c, dw9719_id_table);
```

Without it the VCM never binds, `ov8865`'s async sub-notifier never completes,
`cio2_notifier_complete()` never runs, **no sensor gets a pad link**, and every
camera on the machine — including the IR one — is invisible.

### 2. Start runtime-suspended, and a best-effort lens park (2026-10-02)

Without this, **every system suspend failed** (`pm_runtime_force_suspend
returns -121`): 24 of 27 attempts in 40 days, both the Inateck keyboard's
`KEY_SLEEP` and UPower's critical-battery hibernate. Garden:
`projects/cement-ir/notes/005`. Upstream `master` has the same code, so this
is not a backport.

The chain: ipu-bridge's VCM work item holds the sensor up, creates the VCM
client (which probes **synchronously**), and only *then* adds the
`DL_FLAG_PM_RUNTIME` link VCM→ov8865, without `DL_FLAG_RPM_ACTIVE`. Upstream
dw9719 ended probe runtime-**active** with no link in place, so it held no
reference on the sensor. The sensor suspends, its `VSIO` rail (the VCM's I2C
path) goes off, the first autosuspend's focus ramp NAKs (`Error writing reg
0x0003: -121`), and the device is stuck at `runtime_status=error` with its own
`VCM` rail still on. Every later system suspend re-runs that callback and
aborts.

- Probe powers up to detect, then powers straight down and enables runtime PM
  in the **suspended** state, so the first `open` resumes through the
  now-existing link with the sensor powered first. (Without `CONFIG_PM` the
  chip stays on, as before.)
- `dw9719_suspend()` treats the focus ramp as best effort: on a NAK it stops
  ramping but still powers down and returns the regulator's result. A failed
  park is not a reason to strand the rail on and wedge system sleep. The NAK is
  still logged by `cci_write()`, so a recurrence stays visible.

Why `rmmod`+`modprobe` hid this: on a **rebind** the link already exists, the
driver core resumes suppliers around `probe`, and `pm_runtime_set_active`
takes the link reference properly. So the unpatched code suspends fine after
`cement-ir dkms install`, which is why 2026-09-26, the install day, was the
only day suspend worked, and the bug comes back at the next **cold boot**.
Prove changes here across a cold boot, never right after install.

## Provenance and the numbers that identify it

| | |
|---|---|
| source | `~/dw9719fix/dw9719.c`, 2026-08-21 (the August bring-up) |
| md5 (delta 1 only) | `f862fdb45f02437d2087537f36d2ec1e` |
| built srcversion | `268591B62EE1FD6962113C4` since 2026-10-02 (delta 1 only: `1458708E79A2D2CA601F853`); pinned in `crates/cement-ir/src/dkms.rs` |
| stock srcversion | `7EDAAC55…` — 0 i2c aliases; the mismatch *is* the diagnosis |

`srcversion` is an MD5 over the source, so it is stable across kernel rebuilds
and changes only if this file changes. **If you edit this file, update
`PATCHED_SRCVERSION` in `crates/cement-ir/src/dkms.rs`** — otherwise
`cement-ir dkms verify` will correctly refuse to recognise your own build.

The licence is the kernel's: this is a modified copy of a GPL-2.0 kernel source
file, kept for local rebuilds only. On-device only, by decision — it is not
going upstream (delta 1 already is upstream; linux-surface simply has not shipped
it). Delta 2 is a real upstream bug that is not fixed upstream either; by the same
decision it is carried here only.

## Why not a patch file instead of a whole copy

A `.patch` would need the exact base file to apply against, which changes with
every kernel bump; the whole file is 13 KB and is what DKMS actually compiles.
The deltas against the distro source are small and written out above, so
nothing is lost by storing the result rather than the diff.
