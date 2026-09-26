# The `dw9719` carry — the patched source, under version control

> Last reviewed: 2026-09-26

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

One hunk: the I2C device id table that linux-surface `6.19.8.arch1-3` ships
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

## Provenance and the numbers that identify it

| | |
|---|---|
| source | `~/dw9719fix/dw9719.c`, 2026-08-21 (the August bring-up) |
| md5 | `f862fdb45f02437d2087537f36d2ec1e` — byte-identical to `/usr/src/dw9719-fix-1.0/dw9719.c` as installed |
| built srcversion | `1458708E79A2D2CA601F853` (pinned in `crates/cement-ir/src/dkms.rs`) |
| stock srcversion | `7EDAAC55…` — 0 i2c aliases; the mismatch *is* the diagnosis |

`srcversion` is an MD5 over the source, so it is stable across kernel rebuilds
and changes only if this file changes. **If you edit this file, update
`PATCHED_SRCVERSION` in `crates/cement-ir/src/dkms.rs`** — otherwise
`cement-ir dkms verify` will correctly refuse to recognise your own build.

The licence is the kernel's: this is a modified copy of a GPL-2.0 kernel source
file, kept for local rebuilds only. On-device only, by decision — it is not
going upstream (it already is upstream; linux-surface simply has not shipped it).

## Why not a patch file instead of a whole copy

A `.patch` would need the exact base file to apply against, which changes with
every kernel bump; the whole file is 13 KB and is what DKMS actually compiles.
The delta against the distro source is one hunk and is written out above, so
nothing is lost by storing the result rather than the diff.
