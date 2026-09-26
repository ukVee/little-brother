#!/usr/bin/env python3
"""IR face-capture tooling for the Surface Go 3.

Reached through the `cement-ir` entry point; the subcommands are the same.

  shot   -- one capture at given settings, written as PGM (for tuning)
  sweep  -- a grid of current/gain settings, one PGM each (to pick an operating point)
  feed   -- drive a v4l2loopback device with unpacked 8-bit frames, for Howdy
  torch  -- light/extinguish/inspect the illuminator, on the verified sequence

Everything here must run as root: the torch lives behind /dev/i2c-2.

The torch is turned OFF in a finally block and on SIGTERM/SIGINT.  That matters
more than it looks -- an IR emitter left latched is invisible, draws current,
and 40s at 130mA was enough to raise OVP+UVLO on this PMIC.  'feed' also
self-terminates after --duration so that a PAM stack which never reaches a
cleanup line cannot strand the emitter on.
"""
import argparse
import os
import pathlib
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ip3y
import tps68470 as pmic

IRUP = str(Path(__file__).resolve().parent / "ir-up.sh")

R_WLEDMAXT, R_VWLEDCTL = 0x2D, 0x31
R_WLEDC1, R_WLEDC2, R_WLEDCTL = 0x34, 0x35, 0x36


def torch_on(steps, tries=4):
    """steps of 32.5 mA. Sequencing is load-bearing -- see garden hardware/notes/005.

    The enable is intermittently racy: WLEDCTL reads back 0x01 (MODE=torch,
    EN=0) instead of 0x05, and the emitter stays dark. Measured 3 failures in
    16 captures. So verify EN after writing and retry, rather than assuming the
    write took -- a torch that silently fails during a PAM auth is a failed
    login, and the frame it produces is indistinguishable from a dark room.
    """
    for _ in range(tries):
        pmic.put(R_WLEDCTL, 0x00)      # MAXT is not writable while enabled
        time.sleep(0.02)               # let the boost ramp down before MAXT
        pmic.put(R_WLEDMAXT, steps)
        pmic.put(R_WLEDC1, steps)
        pmic.put(R_WLEDC2, steps)
        pmic.put(R_VWLEDCTL, 0x39)     # reset value | ENABLE
        pmic.put(R_WLEDCTL, 0x01)      # MODE='01' torch FIRST...
        time.sleep(0.01)
        pmic.put(R_WLEDCTL, 0x05)      # ...THEN EN, separate write
        v = pmic.get(R_WLEDCTL)
        if v & 0x04:                   # EN actually latched
            return v
        time.sleep(0.05)
    print(f"  WARNING: torch EN never latched (WLEDCTL=0x{v:02x})", file=sys.stderr)
    return v


def torch_off():
    try:
        pmic.put(R_WLEDCTL, 0x00)
    except Exception:
        pass


def torch_refresh():
    """Torch self-disables after a 13s watchdog unless MODE is refreshed."""
    pmic.put(R_WLEDCTL, 0x01)
    pmic.put(R_WLEDCTL, 0x05)


def graph_up(gain, exposure):
    """Configure the graph; return the CIO2 video node ir-up.sh resolved.

    Never hardcode this. The media graph renumbers across reboots -- the ov7251
    fed ipu3-csi2 2 via /dev/video2 on one boot and /dev/video9 on the next.
    Streaming from the wrong node does not error, it blocks forever.
    """
    env = dict(os.environ, GAIN=str(gain), EXPOSURE=str(exposure))
    r = subprocess.run([IRUP], env=env, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"ir-up.sh failed ({r.returncode}): {r.stderr.strip()[:400]}")
    m = re.search(r"^ready: (/dev/video\d+)", r.stdout, re.M)
    if not m:
        sys.exit("could not parse the video node from ir-up.sh:\n" + r.stdout)
    sd = re.search(r"\((/dev/v4l-subdev\d+)\)", r.stdout)
    return m.group(1), (sd.group(1) if sd else None)


def readback(subdev):
    """Sensor gain/exposure as the hardware actually holds them.

    Two different faults both render as a ~18-mean frame: the torch not lighting,
    and analogue_gain silently reverting to its minimum of 16. Garden
    hardware/notes/005 records the latter wasting a whole dark-room session
    (2026-08-21) in full. Distinguishing them requires
    reading state back AFTER the capture, never just setting it before.
    """
    if not subdev:
        return {}
    r = subprocess.run(["v4l2-ctl", "-d", subdev, "--get-ctrl",
                        "analogue_gain,exposure"], capture_output=True, text=True)
    out = {}
    for line in r.stdout.splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            out[k.strip()] = v.strip()
    return out


def grab(video, count, timeout=30):
    """Return raw ip3y bytes for `count` frames."""
    try:
        r = subprocess.run(
            ["v4l2-ctl", "-d", video, "--stream-mmap",
             f"--stream-count={count}", "--stream-to=-"],
            capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        sys.exit(f"capture from {video} timed out after {timeout}s. That node "
                 f"most likely has no producer -- check the graph with "
                 f"`media-ctl -p -d /dev/media0`.")
    if len(r.stdout) < ip3y.FRAME_BYTES:
        sys.exit(f"capture produced {len(r.stdout)} bytes, need "
                 f"{ip3y.FRAME_BYTES}: {r.stderr.decode()[:300]}")
    return r.stdout


def write_pgm(path, arr8):
    with open(path, "wb") as f:
        f.write(b"P5\n%d %d\n255\n" % (arr8.shape[1], arr8.shape[0]))
        f.write(arr8.tobytes())
    # Everything here runs as root, but the images have to be retrievable by the
    # login user afterwards.
    try:
        os.chown(path, 1000, 1000)
    except OSError:
        pass


def to8(frame10):
    return (frame10 >> 2).astype(np.uint8)


def report(tag, frame10, flag=""):
    hi = float((frame10 >= 1020).mean() * 100)
    print(f"  {tag:28s} min {int(frame10.min()):4d} max {int(frame10.max()):4d} "
          f"mean {float(frame10.mean()):6.1f}  clipped {hi:5.2f}%"
          + (f"   !! {flag}" if flag else ""))
    return hi


def cmd_shot(a):
    video, subdev = graph_up(a.gain, a.exposure)
    torch_on(a.current) if a.current else torch_off()
    try:
        buf = grab(video, a.frames)
    finally:
        rb = readback(subdev); wled = pmic.get(R_WLEDCTL)
        torch_off()
    f = ip3y.frame_at(buf, a.frames - 1)
    report(f"cur={a.current} gain={a.gain} exp={a.exposure}", f)
    write_pgm(a.out, to8(f))
    print(f"  wrote {a.out}")


def cmd_sweep(a):
    outdir = Path(a.outdir)
    outdir.mkdir(parents=True, exist_ok=True)
    try:
        os.chown(outdir, 1000, 1000)
    except OSError:
        pass
    total = len(a.currents) * len(a.gains)
    print(f"Hold your face ~40cm from the camera and keep still for ~{total * 3}s.")
    print("'clipped' is the percentage of pixels at/near saturation -- a face")
    print("that is blown out is useless to dlib, so lower is better there.\n")
    for cur in a.currents:
        for gain in a.gains:
            tag = f"cur{cur}_gain{gain}"
            for attempt in (1, 2):
                video, subdev = graph_up(gain, a.exposure)
                if cur:
                    torch_on(cur)
                    time.sleep(0.25)          # let the boost ramp before capture
                else:
                    torch_off()
                try:
                    buf = grab(video, a.frames)
                finally:
                    rb = readback(subdev)
                    wled = pmic.get(R_WLEDCTL)
                    torch_off()
                f = ip3y.frame_at(buf, a.frames - 1)

                got_gain = int(rb.get("analogue_gain", -1))
                torch_live = bool(wled & 0x04)      # EN
                bad = []
                if got_gain != gain:
                    bad.append(f"GAIN={got_gain}!={gain}")
                if cur and not torch_live:
                    bad.append(f"TORCH-OFF(WLEDCTL=0x{wled:02x})")
                if not bad or attempt == 2:
                    break
                print(f"  {tag:28s} retrying -- {' '.join(bad)}")

            report(tag, f, " ".join(bad) if bad else "")
            write_pgm(outdir / f"{tag}.pgm", to8(f))
            time.sleep(0.3)
    print(f"\n  {len(a.currents) * len(a.gains)} frames in {outdir}")


def cmd_feed(a):
    """Stream unpacked 8-bit frames into a v4l2loopback node until --duration."""
    video, subdev = graph_up(a.gain, a.exposure)
    ff = subprocess.Popen(
        ["ffmpeg", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray",
         "-s", f"{ip3y.WIDTH}x{ip3y.HEIGHT}", "-r", str(a.fps), "-i", "-",
         "-f", "v4l2", "-pix_fmt", "gray", a.device],
        stdin=subprocess.PIPE)
    # Do NOT discard this stderr. When the capture side fails, the only symptom
    # is "fed 0 frames" and the actual reason goes to /dev/null -- which is
    # exactly how a broken IR capture got misdiagnosed as a loopback problem.
    cap_err = open("/tmp/ir-capture-stderr.log", "wb")
    # Bound the frame count to the run instead of using a huge number and then
    # killing v4l2-ctl mid-stream. Terminating during DQBUF leaves the CIO2
    # pipeline and sensor in a bad state -- afterwards STREAMON returns EINVAL
    # and the ov7251 stops answering I2C entirely (EREMOTEIO).
    # Size the count by the SENSOR rate (ov7251 runs at 30fps, per its
    # Y10_1X10/640x480@100/3000 format), not by --fps, which only tells ffmpeg
    # how to timestamp the output. Using --fps here made v4l2-ctl hit its count
    # in a third of the requested duration and the feeder exit mid-enrollment.
    SENSOR_FPS = 35          # 30fps nominal, plus margin
    want = max(16, int(a.duration * SENSOR_FPS) + 50)
    cap = subprocess.Popen(
        ["v4l2-ctl", "-d", video, "--stream-mmap", f"--stream-count={want}",
         "--stream-to=-"], stdout=subprocess.PIPE, stderr=cap_err)

    stop = False

    def _sig(*_):
        nonlocal stop
        stop = True

    signal.signal(signal.SIGTERM, _sig)
    signal.signal(signal.SIGINT, _sig)

    torch_on(a.current)
    deadline = time.monotonic() + a.duration
    last_refresh = time.monotonic()
    n = 0
    try:
        while not stop and time.monotonic() < deadline:
            buf = cap.stdout.read(ip3y.FRAME_BYTES)
            if not buf or len(buf) < ip3y.FRAME_BYTES:
                break
            ff.stdin.write(to8(ip3y.unpack(buf)).tobytes())
            n += 1
            # The feeder's own readiness signal (slice 004): the first frame that
            # ffmpeg actually accepted is the moment a consumer can start reading
            # the loopback. Touch the marker the caller (PAM helper / howdy.sh)
            # waits on -- no second process re-derives what this loop already
            # knows, and nothing imports python-opencv to do it. If ffmpeg is
            # already dead (poll() returns an rc), do not signal: the frame went
            # nowhere.
            if n == 1 and a.ready_file and ff.poll() is None:
                try:
                    pathlib.Path(a.ready_file).touch()
                except OSError:
                    pass
            if time.monotonic() - last_refresh > 8:      # inside the 13s watchdog
                torch_refresh()
                last_refresh = time.monotonic()
    except BrokenPipeError:
        pass
    finally:
        torch_off()
        # The marker lives only as long as this run does.
        if a.ready_file:
            try:
                pathlib.Path(a.ready_file).unlink(missing_ok=True)
            except OSError:
                pass
        # Close the read end first so v4l2-ctl sees EPIPE and unwinds its own
        # streaming teardown, rather than being shot mid-DQBUF.
        try:
            if cap.stdout:
                cap.stdout.close()
        except Exception:
            pass
        try:
            cap.wait(timeout=3)
        except Exception:
            cap.terminate()
            try:
                cap.wait(timeout=2)
            except Exception:
                cap.kill()
        try:
            if ff.stdin:
                ff.stdin.close()
        except Exception:
            pass
        try:
            ff.wait(timeout=3)
        except Exception:
            ff.terminate()
        print(f"  fed {n} frames to {a.device}", file=sys.stderr)
        if n == 0:
            try:
                cap_err.flush()
            except Exception:
                pass
            err = pathlib.Path("/tmp/ir-capture-stderr.log").read_text()[-800:]
            print(f"  capture source was {video}; v4l2-ctl said:\n{err or '  (nothing)'}",
                  file=sys.stderr)
            print(f"  cap exit={cap.poll()} ff exit={ff.poll()}", file=sys.stderr)


def cmd_torch(a):
    """The illuminator as a command, not a raw register poke.

    This replaces the August ir-wled.sh / iledctl.py probes, which each
    re-implemented the enable sequence -- and got it wrong before the retry loop
    above existed. The torch cannot be left latched from here: the hold refreshes
    MODE inside the 13 s watchdog, and main()'s finally turns it off on every exit
    path. An emitter left on is invisible, draws current, and 40 s at 130 mA
    browns out the sensor's own rail (garden hardware/notes/005).

    Slice 005 of the milestone makes this the SOLE owner: hard current and
    duration caps, and refusing to light with OVP|UVLO already latched.
    """
    if a.action == "off":
        torch_off()
        print("  torch off")
        return
    if a.action == "status":
        g, ctl = pmic.get(0x01), pmic.get(R_WLEDCTL)
        print(f"  GSTAT    0x{g:02x}  [{pmic.decode_gstat(g)}]")
        print(f"  WLEDCTL  0x{ctl:02x}  [{pmic.decode_ctl(ctl)}]")
        print(f"  WLEDMAXT 0x{pmic.get(R_WLEDMAXT):02x}  "
              f"WLEDC1 0x{pmic.get(R_WLEDC1):02x}  WLEDC2 0x{pmic.get(R_WLEDC2):02x}")
        return
    v = torch_on(a.current)
    print(f"  torch on: {a.current} x 32.5mA  WLEDCTL=0x{v:02x}  holding {a.hold:.0f}s")
    end = time.monotonic() + a.hold
    while time.monotonic() < end:
        time.sleep(min(6.0, max(0.0, end - time.monotonic())))
        if time.monotonic() < end:
            torch_refresh()
    print(f"  releasing; WLEDCTL=0x{pmic.get(R_WLEDCTL):02x} while lit")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--gain", type=int, default=128)   # dlib scored best here
    ap.add_argument("--exposure", type=int, default=1704)
    ap.add_argument("--current", type=int, default=1,
                    help="torch current in 32.5mA steps; 0 = off")
    sub = ap.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("shot"); s.set_defaults(fn=cmd_shot)
    s.add_argument("--out", default="/tmp/ir.pgm")
    s.add_argument("--frames", type=int, default=6)

    s = sub.add_parser("sweep"); s.set_defaults(fn=cmd_sweep)
    s.add_argument("--outdir", default="/home/ukv/ir-tune")
    s.add_argument("--frames", type=int, default=6)
    s.add_argument("--currents", type=int, nargs="+", default=[0, 1, 2, 4])
    s.add_argument("--gains", type=int, nargs="+", default=[128, 256, 512, 1023])

    s = sub.add_parser("feed"); s.set_defaults(fn=cmd_feed)
    s.add_argument("--device", required=True)
    s.add_argument("--duration", type=float, default=12.0)
    s.add_argument("--fps", type=int, default=15)
    s.add_argument("--ready-file", default=None,
                   help="touch this path once the first frame reaches ffmpeg "
                        "(the feeder's own readiness signal, slice 004)")

    s = sub.add_parser("torch"); s.set_defaults(fn=cmd_torch)
    s.add_argument("action", choices=["on", "off", "status"])
    s.add_argument("--hold", type=float, default=10.0,
                   help="seconds to hold the torch lit; refreshed inside the 13s watchdog")

    a = ap.parse_args()
    if os.geteuid() != 0:
        sys.exit("must run as root (torch control lives behind /dev/i2c-2)")
    try:
        a.fn(a)
    finally:
        torch_off()


if __name__ == "__main__":
    main()
