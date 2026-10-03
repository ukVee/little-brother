# irfaced — the on-demand face-capture service (slice 006 design)

One process owns the sensor, the emitter (through `tps68470`'s `Torch` guard,
in-process) and the loopback. PAM becomes a trigger: the helper connects to a
socket, asks for a frame stream, waits for the readiness reply, exits 0
always. systemd socket-activates the daemon; it stays warm between requests
and exits after an idle timeout, so at rest nothing runs and the sensor stays
unpowered.

## Decisions

### Activation: systemd socket activation (not D-Bus, not `systemctl start`)

- A socket at `/run/cement-ir/irfaced.sock` (root-only, `SocketMode=0600` —
  the PAM helper runs as root via `seteuid`; dotfiles `b869fc9` widened it to
  `0660 root:wheel` for gtklock, and it went back to `0600` once gtklock face
  auth was removed; see the v2 section below). The **connect itself** is the
  trigger: the first auth pays the cold start, the daemon then stays warm
  until the idle timeout. No D-Bus dependency (this machine's zero-dep
  choice), no per-auth `systemctl` round-trip.
- The slice's "exits after an idle timeout" points straight at socket
  activation: the `.socket` unit keeps listening while the service is dead;
  the next connect respawns it.
- `Type=simple`, `Restart=on-failure` (a clean idle exit is 0 → no restart).

### Process shape: one binary (`cement-ir-rt irfaced`), not a second binary

The decision doc's crate sketch had a separate `crates/irfaced/`. Deliberate
deviation, for the decision's own first argument: **"the auth path becomes
one Rust binary."**

- The daemon needs the `Torch` guard, the signal handling and the install
  discipline that already live in `cement-ir-rt`; a second binary would fork
  all three.
- One binary = one `make install` target, one `doctor` freshness check, one
  thing to not run out of `target/` (accepted cost 1). The trigger client
  shares the protocol code in the same crate.
- The daemon lights the torch **in-process** via `tps68470::Torch` (RAII +
  refresh + Drop) — no `torch hold` child, no `--parent` plumbing. That
  plumbing existed because the Python feeder could not own the guard; this
  process is the owner.

### The socket protocol (versioned, fixed 32-byte request)

```
magic u32 (0x46524943 "CIRF" LE) | version u32 (1)
duration_secs u32 | current_steps u8 | gain u16 | exposure u32 | reserved[13]
```

Reply: one line — `ready\n` (first frame on the loopback), `error <reason>\n`
(refusal or bring-up failure), or `starting`/`ready` when another feed is
already running (the old helper's "feeder already running → exit 0" became
"frames are already flowing → say ready"). The trigger client prints the
reply and its own elapsed time (`daemon: ready (139 ms since connect)`), so
Done-when #1's measurement is built into the tool.

Policy checks run **before any hardware is touched** (the slice 005 lesson):
duration 1..=120 (the owner's ceiling), steps 0..=3 (0 = no illuminator,
legal for a feed), gain 1..=1023. A refusal is one line, needs no root.

### Protocol v2: the `frames` verb (fid-gate slice 003, 2026-10-03)

`facial-id-defender`'s gate judges presence from IR frames, and the sensor
keeps one owner. So v2 lets a **root** caller ask for N frames back on the
socket instead of on the loopback. Same capture path, same operating point,
same teardown. Recognition stays out of this process.

```
magic u32 | version u32 (2) | duration_secs u32 (0) | current_steps u8 |
gain u16 | exposure u32 | kind u8 (1 = frames) | count u8 (1..=10) | reserved[11] (zero)
```

- **A feed is still sent as v1**, byte for byte. The PAM helper's request is
  unchanged, so a warm daemon still running the old binary keeps serving auth
  (`make install` stops it anyway). v1 never checked bytes 19..32 and still
  doesn't. v2 is strict: reserved bytes zero, `count` only on frames,
  `duration` only on a feed.
- **Reply:** `frames <n>\n`, then `n` records of a 24-byte header (`len u32 |
  width u16 | height u16 | mono_ns u64 | mean f32 | darkness f32`, LE) and
  `len` bytes of 640x480 8-bit grey (the same `>> 2` the loopback gets).
  - `mono_ns` is `CLOCK_MONOTONIC` when the daemon dequeued the frame.
  - `darkness` is the **raw** first-bin share: 100 × the share of pixels in
    the first of 8 histogram bins over 0..256. That is `howdy-compare`'s
    `cv::calcHist` histogram, but taken on the frame as captured. howdy
    applies CLAHE first, so its `Average darkness` for the same scene reads
    lower: 71.16 against 92.1 (garden: fid-gate slice 004, Block 3,
    2026-10-03). Comparing this value with `dark_threshold` is wrong. A
    consumer that needs howdy's number takes it after CLAHE
    (facial-id-defender's `fid-face` does).
  - Or one `error <code> (<detail>)` line. The codes are `invalid`,
    `forbidden`, `busy`, `preempted`, `capture`, `torch`, `short` and
    `stopping`.
- **Root only, for every request, by `SO_PEERCRED`.** This is the owner's
  decision of 2026-10-03: the sensor is exclusive to its controllers.
  - The controllers are the PAM helper (root via `pam_exec seteuid`;
    `SO_PEERCRED` reports the EUID) and `fid-gated` (root). Anything
    unprivileged borrows through `fid-gated`, never this socket.
  - The daemon's own user is also allowed, so a developer's daemon on a
    private socket serves its developer.
  - The peer is judged **before a byte of the request is read**. A refused
    peer gets `error forbidden (irfaced serves root only)` at once, so it
    cannot hold the single-threaded loop for the 2 s read timeout while an
    auth waits.
  - The socket's file mode says the same thing (`0600` in a `0700` directory, dotfiles `fd0b614`),
    but a unit edit loosened it once (`b869fc9`), so the daemon enforces it
    itself.
- **Auth always wins.** A frames request during an auth feed is refused
  `busy` without touching the sensor. An auth trigger during a frames capture
  ends it at once: its caller gets `preempted` and the feed starts. The
  capture is driven tick by tick from the event loop like a feed, so the
  trigger waits at most one frame interval, plus the teardown.
- **Bounded.** The capture must finish within `FRAMES_DEADLINE_MS` (2 s), or
  it gets `short`, or `capture` with zero frames. That is well inside the
  torch watchdog, so there is no MODE refresh. The reply write is bounded by
  `SEND_DEADLINE_MS` (250 ms) in total, so a client that never reads cannot
  hold up an auth. The torch is out and the streams are off **before** the
  reply is written.
- **Fault accounting is shared** (`faults::WEDGE`, the a8224c2 rule).
  - Counted: a wedge-signature errno, zero frames by the deadline with
    nothing else to explain it, and teardown errors.
  - Not counted: `EBUSY`, a refusal, a preemption or a short capture. None of
    them can arm the rebind.
- **No frame reaches disk or the journal.** The journal gets counts and
  timings only, not even a frame's mean. The daemon sets `RLIMIT_CORE` to 0:
  `systemd-coredump` honours that and stores nothing, while
  `PR_SET_DUMPABLE` would not, under `fs.suid_dumpable = 2`.

`cement-ir-rt irfaced frames --count 3` is the reference client. It prints
each frame's offset, mean and darkness, re-derives both from the pixels to
check the transfer, and drops the pixels.

### Teardown ordering (inherited from `irface.py cmd_feed`)

1. **Torch out first** — the illuminator must never outlive the feed by one
   slow step of teardown.
2. **Capture STREAMOFF** (the producer) — clean, in-process; nothing is ever
   killed mid-DQBUF, which is what used to wedge the sensor (STREAMON EINVAL,
   I2C EREMOTEIO). The frame-count bound survives as defence in depth
   (`duration × 35 fps + 50`, exactly `cmd_feed`'s `want`).
3. **Loopback STREAMOFF** (the consumer).
4. **Control readback** — `analogue_gain`/`exposure` read back and journaled
   around every capture (the 2026-08-21 near-black trap: two faults render
   identically without the readback).

The old EPIPE dance (close the read end so `v4l2-ctl` unwinds) does not
exist here: there is no subprocess, so there is no pipe to close — STREAMOFF
is the unwind, and it is always clean.

### What the helper became

`/usr/local/bin/howdy-ir-pam` keeps its ssh ancestry walk, its root check,
its log fallback chain and its always-exit-0 contract, and drops everything
else: the repo path (and with it the staleness class doctor's old check
existed to catch), the per-auth `sensor-recover.sh` rebind (the wedge handler
leaves the happy path; slice 007 soaks that), the `doctor --quick` preflight
(the daemon reports its own failures with strictly better evidence — the
helper logs the socket reply), and the pgrep guard (the daemon serialises).
It is one call: `cement-ir-rt irfaced trigger --duration 15 --steps 1 --gain
128 --exposure 1704 --timeout 5`.

### Observability

Everything goes to the journal (systemd `StandardOutput=journal`): the
request, the resolved graph (`sensor … -> csi2 … -> /dev/videoN`), the
readiness moment, the frame count, the readback, and every failure with its
actual reason — the exact opposite of the old `/dev/null` + `/tmp` discard.

### Bombadil

> **Superseded 2026-10-03.** These files are no longer bombadil symlinks. A
> symlink from `/etc` or `/usr/local/bin` into `~/dotfiles` (plus the ACLs
> bombadil needed to create it) let any process running as ukv edit what root
> runs. The helper runs as root on every sudo, so editing it was a route to
> root with no password. They are now **root-owned copies** installed by
> `sudo dotfiles-deploy-system` from `~/dotfiles/src/system/deploy.manifest`,
> and `doctor` fails a symlink (`deployed_state`, `lib/common.sh`). The source
> paths below are unchanged. Garden: `services/notes/001`,
> `projects/facial-id-defender/` P0a.

The unit, the socket, the helper and both v4l2loopback confs become
bombadil-sourced (precedent: `acpi_lid_action → /usr/local/bin/lid-close.sh`):

```
src/system/systemd/irfaced/irfaced.socket  → /etc/systemd/system/irfaced.socket
src/system/systemd/irfaced/irfaced.service → /etc/systemd/system/irfaced.service
src/system/cement-ir/howdy-ir-pam          → /usr/local/bin/howdy-ir-pam
src/system/modprobe.d/v4l2loopback.conf    → /etc/modprobe.d/v4l2loopback.conf
src/system/modules-load.d/v4l2loopback.conf → /etc/modules-load.d/v4l2loopback.conf
```

`/etc/pam.d/sudo` stays hand-edited (distro file; a symlink invites a
`.pacnew` mess). The new helper has **no** template variables — it needs no
repo path, so `cement-ir pam install-helper` and its whole staleness check
are deleted; `doctor`'s helper check now verifies the bombadil symlink + the
trigger call instead.

## Two ABI lessons this slice learned on the device

Both are pinned in `crates/irctl` with tests and comments:

1. **V4L2 node names are not char-device minors.** `/dev/v4l-subdev7` has
   minor 22 on this kernel; `/dev/video2` has minor 3 (the v4l2 core numbers
   video nodes, subdevs and minors from different counters). Any code that
   derives `/dev/video$minor` or `/dev/v4l-subdev$minor` is wrong by
   construction. The fix, which is what media-ctl does: resolve through
   `/sys/dev/char/<major>:<minor>` → the basename of that symlink IS the node
   name (`media::node_name`). This is deeper than the renumbering lesson
   already in `ir-up.sh`; both are now one trap.
2. **The CIO2 node is multiplanar-only.** `S_FMT`/`REQBUFS` with
   `V4L2_BUF_TYPE_VIDEO_CAPTURE` fail EINVAL while `v4l2-ctl` succeeds —
   because v4l2-ctl reads `VIDIOC_QUERYCAP` and switches to the mplane type
   and the `pix_mp` payload. `video::Stream::start` does the same detection
   (single-planar for the v4l2loopback sink, mplane for the CIO2 capture),
   including the per-plane `v4l2_plane` arrays on DQBUF/QBUF.
   (The earlier legacy-`MEDIA_IOC_ENUM_ENTITIES` attempt also showed its
   `dev` union to be unreliable on this kernel — the v2 `G_TOPOLOGY` API is
   the enumeration path, per the kernel header's own deprecation note.)

## Verified live on this device as `ukv` (no root, steps=0)

- `irfaced resolve` output matches `ir-up.sh` line for line (sensor, csi2 2,
  `/dev/video2`, subdev7).
- Daemon + trigger end-to-end: `daemon: ready (139 ms since connect)` against
  the 6–7 s baseline; 88 frames fed in 3 s (~29 fps ≈ the sensor rate);
  readback `gain=128 exposure=1704`.
- A v4l2-ctl reader pulled 3 × 307 200 bytes from `/dev/video20` mid-feed.
- After the run: a fresh `ir-up.sh --capture` succeeded (clean teardown, no
  wedge) and the daemon exited at the idle timeout.
- Not verified (root + tablet → `## Deferred verification` D11–D15): the
  torch path in-process, the <1 s × 10 cold triggers with the illuminator,
  the systemd units, the bombadil reproduction, the forced-failure journal
  line.

## Not in this slice (deliberately)

- The Python feeder stays in the tree until the daemon proves itself on the
  auth path (the Rust line: nothing cut over on faith).
- `sensor-recover.sh` stays as the manual `cement-ir recover` fault handler;
  demoting it from the happy path lands its burden on the clean STREAMOFF +
  slice 007's soak.
- No presence/wake code (locked decision 4); no PAM surface change beyond
  the helper it already runs.
