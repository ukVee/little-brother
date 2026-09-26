//! `irfaced` — the on-demand face-capture daemon (slice 006) and its trigger
//! client, both modes of the one runtime binary `cement-ir-rt`.
//!
//! One process owns the sensor, the loopback and (through `tps68470`'s
//! `Torch` guard, in-process — no child) the illuminator. PAM becomes a
//! trigger: the helper connects to the socket, asks for a frame stream, waits
//! for the readiness reply, and exits 0 always. The daemon is socket-activated
//! by systemd, stays warm between requests, and exits after an idle timeout —
//! so at rest nothing runs and the sensor stays unpowered.
//!
//! Design decisions + teardown ordering: `docs/irfaced-design.md`.
//!
//! # The readiness reply
//!
//! Slice 004's readiness gate was a file the Python feeder touched after the
//! first frame reached ffmpeg. Here the daemon writes `ready\n` on the client
//! socket after the first unpacked frame has been **written to the loopback**
//! — strictly stronger evidence (the frame really reached the device), same
//! contract: the helper waits for it before letting `pam_howdy` proceed.
//!
//! # Teardown ordering (inherited from `lib/irface.py cmd_feed`)
//!
//! 1. release the torch first (the illuminator must never outlive the feed
//!    by even one slow step of teardown);
//! 2. STREAMOFF the capture (the producer) — clean, in-process, nothing is
//!    ever killed mid-DQBUF, which is what used to wedge the sensor;
//! 3. STREAMOFF the loopback sink (the consumer);
//! 4. read the sensor controls back and journal everything.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use irctl::{fourcc, media, subdev, video};
use tps68470::bus::Bus;
use tps68470::torch::{self, Torch};

use crate::signals;

pub const SOCKET_PATH: &str = "/run/cement-ir/irfaced.sock";
pub const REQ_MAGIC: u32 = 0x4652_4943; // "CIRF" little-endian
pub const REQ_VERSION: u32 = 1;
pub const REQ_BYTES: usize = 32;
pub const DEFAULT_FEED_SECS: u32 = 15;
pub const MAX_FEED_SECS: u32 = 120; // = the torch owner's duration ceiling
pub const DEFAULT_IDLE_SECS: u64 = 120;
pub const DEFAULT_GAIN: u16 = 128; // dlib scored best here (irface.py)
pub const DEFAULT_EXPOSURE: u32 = 1704;
pub const DEFAULT_STEPS: u8 = 1;
/// ov7251 runs at 30 fps nominal (Y10_1X10/640x480@100/3000); 35 with margin,
/// carried verbatim from `cmd_feed`. In-process the deadline is the primary
/// bound (nothing gets killed mid-DQBUF here), but the count bound survives as
/// defence in depth: a misbehaving sensor that floods frames ends the feed at
/// `duration * SENSOR_FPS + 50` — exactly `cmd_feed`'s `want`.
pub const SENSOR_FPS: u32 = 35;
/// Torch MODE refresh cadence, inside the ~13 s hardware watchdog.
const TORCH_REFRESH_SECS: u64 = 10;
/// Poll chunk of the daemon event loop — short, so signals and deadlines are
/// noticed within ~200 ms (the slice 005 lesson).
const TICK_MS: i32 = 100;
const NBUFS: u32 = 4;

/// The trigger request, fixed 32 bytes little-endian on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub duration_secs: u32,
    pub current_steps: u8,
    pub gain: u16,
    pub exposure: u32,
}

impl Default for Request {
    fn default() -> Self {
        Request {
            duration_secs: DEFAULT_FEED_SECS,
            current_steps: DEFAULT_STEPS,
            gain: DEFAULT_GAIN,
            exposure: DEFAULT_EXPOSURE,
        }
    }
}

impl Request {
    pub fn encode(&self) -> [u8; REQ_BYTES] {
        let mut b = [0u8; REQ_BYTES];
        b[0..4].copy_from_slice(&REQ_MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&REQ_VERSION.to_le_bytes());
        b[8..12].copy_from_slice(&self.duration_secs.to_le_bytes());
        b[12] = self.current_steps;
        b[13..15].copy_from_slice(&self.gain.to_le_bytes());
        b[15..19].copy_from_slice(&self.exposure.to_le_bytes());
        // b[19..32] reserved, zero
        b
    }

    pub fn decode(buf: &[u8; REQ_BYTES]) -> Result<Request, String> {
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if magic != REQ_MAGIC {
            return Err(format!("bad magic 0x{magic:08x} (not a cement-ir trigger)"));
        }
        if version != REQ_VERSION {
            return Err(format!("unsupported protocol version {version}"));
        }
        Ok(Request {
            duration_secs: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            current_steps: buf[12],
            gain: u16::from_le_bytes(buf[13..15].try_into().unwrap()),
            exposure: u32::from_le_bytes(buf[15..19].try_into().unwrap()),
        })
    }

    /// Policy checks run **before any hardware is touched** — the slice 005
    /// lesson: a refusal costs nothing and needs no root. The steps ceiling is
    /// re-checked inside `Torch::on` (defence in depth); this early check is
    /// what makes the refusal a one-line reply instead of a daemon-side error.
    pub fn validate(&self) -> Result<(), String> {
        if self.duration_secs == 0 || self.duration_secs > MAX_FEED_SECS {
            return Err(format!(
                "duration {}s is outside 1..={MAX_FEED_SECS} (the owner's own clock bounds every feed)",
                self.duration_secs
            ));
        }
        // 0 steps = no illuminator (legal for a feed — dark frames); 1..=3
        // go through the owner's own ceiling check. >3 is refused by the
        // owner before any hardware is touched.
        if self.current_steps > 0 {
            if let Err(e) = torch::check_steps(self.current_steps) {
                return Err(e.to_string());
            }
        }
        if self.gain == 0 || self.gain > 1023 {
            return Err(format!("gain {} is outside 1..=1023", self.gain));
        }
        Ok(())
    }
}

/// The resolved + configured IR capture path (see `media::find_ir_path`).
#[derive(Debug)]
pub struct Graph {
    pub sensor: String,
    pub csi2: String,
    pub video: PathBuf,
    pub subdev: Option<PathBuf>,
}

/// Resolve by entity name and configure the graph — the `lib/ir-up.sh`
/// discipline, in Rust. Never hardcode a node number; see `find_ir_path`.
fn bring_up(media_path: &Path) -> Result<Graph, String> {
    let f = File::open(media_path)
        .map_err(|e| format!("cannot open {}: {e}", media_path.display()))?;
    media::check_pad_index_api(&f).map_err(|e| e.to_string())?;
    let topo = media::get_topology(&f).map_err(|e| format!("get topology: {e}"))?;
    let path = media::find_ir_path(&topo)?;

    // Device nodes come from sysfs (see media::node_name): a V4L2 node's name
    // number is NOT its char minor.
    let video_node = node_or_err(path.video, "CIO2 video")?;
    let subdev_node = match path.subdev {
        Some(d) => Some(node_or_err(d, "sensor subdev")?),
        None => None,
    };

    // Enable the sensor -> CSI-2 link (media-ctl -l ... [1]).
    media::setup_link(&f, path.sensor_pad, path.csi2_pad, media::LNK_FL_ENABLED)
        .map_err(|e| format!("enable link {}:0 -> {}:0: {e}", path.sensor, path.csi2))?;

    // Media-bus formats on both pads (media-ctl -V ... fmt:Y10_1X10/640x480).
    let sdf = File::open(subdev_node.as_ref().unwrap())
        .map_err(|e| format!("open {}: {e}", subdev_node.as_ref().unwrap()))?;
    subdev::set_format(&sdf, 0, fourcc::MEDIA_BUS_FMT_Y10_1X10, 640, 480)
        .map_err(|e| format!("set fmt on {}: {e}", subdev_node.as_ref().unwrap()))?;

    if let Some((maj, min)) = media::subdev_devnode_of(&topo, path.csi2_pad.entity) {
        if let Some(csd) = media::node_name(maj, min) {
            let csif = File::open(&csd).map_err(|e| format!("open {csd}: {e}"))?;
            subdev::set_format(&csif, 0, fourcc::MEDIA_BUS_FMT_Y10_1X10, 640, 480)
                .map_err(|e| format!("set fmt on {csd}: {e}"))?;
        }
    }

    // The CIO2 capture format is set by Stream::start when the feed opens the
    // node (it picks single-planar vs mplane from VIDIOC_QUERYCAP, like
    // v4l2-ctl). Nothing to do here — bring_up's job is the media graph.

    Ok(Graph {
        sensor: path.sensor,
        csi2: path.csi2,
        video: PathBuf::from(video_node),
        subdev: subdev_node.map(PathBuf::from),
    })
}

fn node_or_err(dev: (u32, u32), what: &str) -> Result<String, String> {
    media::node_name(dev.0, dev.1)
        .ok_or_else(|| format!("could not resolve the {what} node for {dev:?} (sysfs missing?)"))
}

enum FeedState {
    /// Frames still flowing (or waiting for the next one).
    Running,
    /// Deadline / signal / error: teardown done, feed over.
    Done,
}

/// One in-flight capture session: the torch guard, both streams, and the
/// client socket that gets the `ready` reply.
struct Feed {
    req: Request,
    client: UnixStream,
    graph: Graph,
    cap: video::Stream,
    sink: video::Stream,
    torch: Option<Torch>,
    deadline: Instant,
    since_refresh: Instant,
    frames: u32,
    ready_sent: bool,
}

fn send_line(stream: &mut UnixStream, line: &str) {
    let _ = stream.write_all(line.as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

impl Feed {
    /// Build the whole capture session. On **any** failure the client gets a
    /// one-line `error <reason>` reply (which the helper logs; auth falls
    /// through to the password) and the same chain goes to the journal — the
    /// slice's "forced failure names the real reason" evidence.
    fn start(
        req: Request,
        mut client: UnixStream,
        media_path: &Path,
        loopback: &str,
    ) -> Result<Feed, ()> {
        let fail = |client: &mut UnixStream, msg: &str| {
            send_line(client, &format!("error {msg}"));
            eprintln!("irfaced: cannot serve: {msg}");
        };
        eprintln!(
            "irfaced: request dur={}s steps={} gain={} exposure={}",
            req.duration_secs, req.current_steps, req.gain, req.exposure
        );
        let graph = match bring_up(media_path) {
            Ok(g) => g,
            Err(msg) => {
                fail(&mut client, &msg);
                return Err(());
            }
        };
        eprintln!(
            "irfaced: sensor {} -> {} -> {}",
            graph.sensor,
            graph.csi2,
            graph.video.display()
        );

        // Gain/exposure MUST be set right before capture, every time
        // (analogue_gain reverts to its minimum of 16 -> near-black frames;
        // the 2026-08-21 trap, garden hardware/notes/005). ir-up.sh warns and
        // continues if the subdev is unresolvable; keep that behaviour.
        if let Some(sd) = &graph.subdev {
            if let Ok(sdf) = File::open(sd) {
                for (id, name, val) in [
                    (subdev::CID_ANALOGUE_GAIN, "analogue_gain", req.gain as i32),
                    (subdev::CID_EXPOSURE, "exposure", req.exposure as i32),
                ] {
                    if let Err(e) = subdev::set_ctrl(&sdf, id, val) {
                        eprintln!("irfaced: WARNING: set {name}={val} failed: {e} (frames may be near-black)");
                    }
                }
            } else {
                eprintln!("irfaced: WARNING: could not open {}; gain/exposure NOT set", sd.display());
            }
        }

        let cap = match video::Stream::start(
            graph.video.to_str().unwrap(),
            true,
            &video::ip3y_640x480(),
            NBUFS,
        ) {
            Ok(c) => c,
            Err(e) => {
                fail(&mut client, &format!("capture {}: {e}", graph.video.display()));
                return Err(());
            }
        };
        let sink = match video::Stream::start(loopback, false, &video::grey_640x480(), NBUFS) {
            Ok(s) => s,
            Err(e) => {
                fail(&mut client, &format!("loopback {loopback}: {e}"));
                return Err(());
            }
        };

        // The torch is lit through the one owner, in-process: the same
        // guard RAII (Drop clears WLEDCTL), the same GSTAT pre-check refusal,
        // the same hard caps — but no child process and no --parent plumbing,
        // because THIS process is the owner now. A refusal here ends the feed
        // like cmd_feed's sys.exit did (dark frames are worse than a
        // password fallthrough with a reason).
        let torch = if req.current_steps > 0 {
            match Bus::open().and_then(|bus| Torch::on(bus, req.current_steps, 4).map_err(io::Error::other))
            {
                Ok(t) => Some(t),
                Err(e) => {
                    fail(&mut client, &format!("torch refused: {e}"));
                    return Err(());
                }
            }
        } else {
            None
        };

        Ok(Feed {
            req,
            client,
            graph,
            cap,
            sink,
            torch,
            deadline: Instant::now() + Duration::from_secs(req.duration_secs as u64),
            since_refresh: Instant::now(),
            frames: 0,
            ready_sent: false,
        })
    }

    /// Pump one tick of the feed. Bounded work; the caller drives it from the
    /// event loop so new connections still get accepted mid-feed.
    fn drive(&mut self) -> FeedState {
        if signals::SIGNALLED.load(Ordering::SeqCst) || Instant::now() >= self.deadline {
            self.finish();
            return FeedState::Done;
        }
        // Defence in depth: the frame-count bound cmd_feed sized by the SENSOR
        // rate — a flooding sensor ends the feed instead of spinning.
        if self.frames >= self.req.duration_secs * SENSOR_FPS + 50 {
            eprintln!("irfaced: frame bound reached ({} frames); ending the feed", self.frames);
            self.finish();
            return FeedState::Done;
        }
        // Refresh the torch MODE inside the ~13 s hardware watchdog.
        if let Some(t) = &self.torch {
            if self.since_refresh.elapsed() >= Duration::from_secs(TORCH_REFRESH_SECS) {
                if let Err(e) = t.refresh() {
                    eprintln!("irfaced: MODE refresh failed: {e}; ending the feed");
                    self.finish();
                    return FeedState::Done;
                }
                self.since_refresh = Instant::now();
            }
        }
        // One capture -> unpack -> loopback hop, if a frame is ready.
        let Self { cap, sink, client, frames, ready_sent, graph, .. } = self;
        let result = cap.with_frame(TICK_MS, |frame| {
            let mut px = vec![0u16; ip3y::PIXELS];
            ip3y::unpack(frame, &mut px);
            // 10-bit -> 8-bit (the >>2 the Python feeder used).
            let grey: Vec<u8> = px.iter().map(|v| (v >> 2) as u8).collect();
            match sink.write_frame(&grey, TICK_MS) {
                Ok(true) => {
                    *frames += 1;
                    if !*ready_sent {
                        // First frame really reached the loopback: the
                        // readiness signal (slice 004's gate, now a socket
                        // line). Reply and keep feeding for the rest of the
                        // requested duration.
                        send_line(client, "ready");
                        *ready_sent = true;
                        eprintln!(
                            "irfaced: ready — first frame on the loopback ({} <- {})",
                            graph.video.display(),
                            graph.sensor
                        );
                    }
                }
                Ok(false) => { /* reader not consuming; drop the frame */ }
                Err(e) => return Err(e),
            }
            Ok(())
        });
        match result {
            Ok(_) => FeedState::Running,
            Err(e) => {
                eprintln!("irfaced: feed error after {} frames: {e}", *frames);
                self.finish();
                FeedState::Done
            }
        }
    }

    /// Teardown in the learned order — see the module doc. Never returns an
    /// error: every step reports its own, and the feed is over either way.
    fn finish(&mut self) {
        // 1. Torch out FIRST: the illuminator must not outlive the feed.
        if let Some(mut t) = self.torch.take() {
            if let Err(e) = t.off() {
                eprintln!("irfaced: torch release failed: {e} (the ~13 s hardware watchdog covers this)");
            } else {
                eprintln!("irfaced: torch released");
            }
        }
        // 2. Capture STREAMOFF (producer first) — clean, in-process, nothing
        //    killed mid-DQBUF (the wedge never happens; STREAMON stays valid).
        if let Err(e) = self.cap.stop() {
            eprintln!("irfaced: capture STREAMOFF: {e}");
        }
        // 3. Loopback STREAMOFF (consumer).
        if let Err(e) = self.sink.stop() {
            eprintln!("irfaced: loopback STREAMOFF: {e}");
        }
        // 4. Read the controls back — two faults render identically as a
        //    ~18-mean frame (torch off vs gain reverted to 16); the readback
        //    is what tells them apart (garden hardware/notes/005).
        let mut rb = String::from("gain/exposure readback skipped (no subdev)");
        if let Some(sd) = &self.graph.subdev {
            if let Ok(sdf) = File::open(sd) {
                let g = subdev::get_ctrl(&sdf, subdev::CID_ANALOGUE_GAIN).map(|v| v.to_string());
                let x = subdev::get_ctrl(&sdf, subdev::CID_EXPOSURE).map(|v| v.to_string());
                rb = format!(
                    "gain={} exposure={}",
                    g.as_deref().unwrap_or("?"),
                    x.as_deref().unwrap_or("?")
                );
            }
        }
        let dur = self.req.duration_secs;
        eprintln!(
            "irfaced: fed {} frames from {} ({} -> {}); {rb}; dur={}s steps={}",
            self.frames,
            self.graph.video.display(),
            self.graph.sensor,
            self.graph.csi2,
            dur,
            self.req.current_steps
        );
    }
}

/// The systemd handoff: LISTEN_FDS=1 hands the bound socket over as fd 3.
fn systemd_listener() -> Option<UnixListener> {
    let fds = std::env::var("LISTEN_FDS").ok()?;
    if fds.trim() != "1" {
        return None;
    }
    let pid = std::env::var("LISTEN_PID").ok()?;
    // SAFETY: getpid() is always safe; the wrapper just isn't marked.
    if pid.trim() != unsafe { libc::getpid() }.to_string() {
        return None;
    }
    // SAFETY: systemd guarantees fd 3 is a listening socket for this unit.
    Some(unsafe { UnixListener::from_raw_fd(3) })
}

/// Bind the manual path (IRFACED_SOCKET, for tests and out-of-systemd runs).
fn bind_listener(path: &Path) -> io::Result<UnixListener> {
    // Unlink only a stale SOCKET (never a regular file) left by a crash.
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_socket() {
            std::fs::remove_file(path)?;
        }
    }
    let l = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Handle one trigger connection: parse + validate, then either attach to the
/// running feed (frames are already flowing — reply ready at once) or start a
/// fresh feed (which owns the socket from here and replies when ready).
fn handle_connection(
    stream: UnixStream,
    feed: &mut Option<Feed>,
    media_path: &Path,
    loopback: &str,
) {
    let mut stream = stream;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; REQ_BYTES];
    if let Err(e) = stream.read_exact(&mut buf) {
        eprintln!("irfaced: dropped a connection: {e}");
        return;
    }
    let req = match Request::decode(&buf).and_then(|r| r.validate().map(|_| r)) {
        Ok(r) => r,
        Err(msg) => {
            send_line(&mut stream, &format!("error {msg}"));
            eprintln!("irfaced: refused a request: {msg}");
            return;
        }
    };
    if let Some(f) = feed.as_mut() {
        // Another auth attempt while the feed runs: the frames are already
        // flowing, which is exactly what the helper's old "feeder already
        // running -> exit 0" short-circuit meant. Do not restart the feed.
        if f.ready_sent {
            send_line(&mut stream, "ready");
        } else {
            send_line(&mut stream, "starting");
        }
        return;
    }
    // Feed::start replies "error <reason>" on the socket itself on any
    // failure; on success the socket is the feed's ready-reply channel.
    if let Ok(f) = Feed::start(req, stream, media_path, loopback) {
        *feed = Some(f);
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// `cement-ir-rt irfaced daemon` — run the socket-activated service.
pub fn daemon_run() -> std::process::ExitCode {
    let media_path = PathBuf::from(env_or("IRFACED_MEDIA", "/dev/media0"));
    let loopback = env_or("IRFACED_LOOPBACK", "/dev/video20");
    let idle_secs: u64 = env_or("IRFACED_IDLE_SECS", &DEFAULT_IDLE_SECS.to_string())
        .parse()
        .unwrap_or(DEFAULT_IDLE_SECS);

    signals::install();
    let listener = match systemd_listener() {
        Some(l) => {
            eprintln!("irfaced: got the socket from systemd (LISTEN_FDS=1)");
            l
        }
        None => {
            let path = PathBuf::from(env_or("IRFACED_SOCKET", SOCKET_PATH));
            match bind_listener(&path) {
                Ok(l) => {
                    eprintln!("irfaced: listening on {} (no systemd handoff)", path.display());
                    l
                }
                Err(e) => {
                    eprintln!("irfaced: cannot listen: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            }
        }
    };
    listener.set_nonblocking(true).expect("set_nonblocking on the listen socket");
    eprintln!(
        "irfaced: media={} loopback={loopback} idle_timeout={idle_secs}s",
        media_path.display()
    );

    let mut feed: Option<Feed> = None;
    let mut last_activity = Instant::now();
    loop {
        if signals::SIGNALLED.load(Ordering::SeqCst) {
            eprintln!("irfaced: signal received; releasing and exiting");
            break;
        }
        // Drive the active feed first (short, bounded), then accept.
        let mut feed_done = false;
        if let Some(f) = feed.as_mut() {
            if let FeedState::Done = f.drive() {
                feed_done = true;
            }
        }
        if feed_done {
            feed = None;
            last_activity = Instant::now();
        }
        match listener.accept() {
            Ok((stream, _)) => {
                last_activity = Instant::now();
                handle_connection(stream, &mut feed, &media_path, &loopback);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => eprintln!("irfaced: accept: {e}"),
        }
        if feed.is_none() && last_activity.elapsed() >= Duration::from_secs(idle_secs) {
            eprintln!(
                "irfaced: idle for {idle_secs}s; exiting (the socket unit reactivates on the next connect)"
            );
            break;
        }
    }
    if let Some(mut f) = feed.take() {
        f.finish(); // signal path: release everything before exiting
    }
    std::process::ExitCode::SUCCESS
}

/// The `cement-ir-rt irfaced trigger` client — what the PAM helper calls.
pub fn trigger_run(
    duration_secs: u32,
    steps: u8,
    gain: u16,
    exposure: u32,
    timeout_secs: u64,
    socket: &str,
) -> std::process::ExitCode {
    let req = Request { duration_secs, current_steps: steps, gain, exposure };
    if let Err(msg) = req.validate() {
        println!("daemon: {msg}");
        return std::process::ExitCode::SUCCESS; // the helper logs this and exits 0 — never block auth
    }
    let started = Instant::now();
    let mut stream = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(e) => {
            println!("daemon: unreachable: {e} (auth falls through to the password)");
            return std::process::ExitCode::SUCCESS;
        }
    };
    if let Err(e) = stream.write_all(&req.encode()) {
        println!("daemon: send failed: {e}");
        return std::process::ExitCode::SUCCESS;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(timeout_secs)));
    let mut reply = String::new();
    let mut buf = [0u8; 512];
    match stream.read(&mut buf) {
        Ok(0) => println!("daemon: closed without a reply (auth falls through)"),
        Ok(n) => {
            reply = String::from_utf8_lossy(&buf[..n]).trim().to_string();
        }
        Err(e) => println!("daemon: no reply within {timeout_secs}s: {e}"),
    }
    let ms = started.elapsed().as_millis();
    if reply == "ready" {
        println!("daemon: ready ({ms} ms since connect)");
    } else {
        println!("daemon: {reply} ({ms} ms)");
    }
    std::process::ExitCode::SUCCESS
}

/// `cement-ir-rt irfaced resolve` — resolve + configure the graph, print the
/// result, do not stream. The verification surface for the by-name resolution
/// (mirrors `cement-ir up`'s output).
pub fn resolve_run(media_path: &str) -> std::process::ExitCode {
    match bring_up(Path::new(media_path)) {
        Ok(g) => {
            println!("sensor: {}  ->  {}", g.sensor, g.csi2);
            println!("ready: {} (ip3y 640x480)", g.video.display());
            match &g.subdev {
                Some(sd) => println!("sensor ctrls node: {}", sd.display()),
                None => println!("WARNING: no subdev node for gain/exposure"),
            }
            std::process::ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("cement-ir-rt: {msg}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = Request { duration_secs: 15, current_steps: 1, gain: 128, exposure: 1704 };
        let buf = r.encode();
        assert_eq!(buf.len(), REQ_BYTES);
        assert_eq!(Request::decode(&buf).unwrap(), r);
    }

    #[test]
    fn request_rejects_bad_magic_and_version() {
        let mut buf = Request::default().encode();
        buf[0] ^= 0xff;
        assert!(Request::decode(&buf).unwrap_err().contains("magic"));
        let mut buf = Request::default().encode();
        buf[7] = 99;
        assert!(Request::decode(&buf).unwrap_err().contains("version"));
    }

    #[test]
    fn policy_checks_precede_hardware() {
        // Duration outside 1..=120, steps above the owner's ceiling, gain 0:
        // all refused before any device is touched.
        assert!(Request { duration_secs: 0, ..Default::default() }.validate().is_err());
        assert!(Request { duration_secs: 121, ..Default::default() }.validate().is_err());
        assert!(Request { current_steps: 4, ..Default::default() }.validate().is_err());
        assert!(Request { current_steps: 0, ..Default::default() }.validate().is_ok());
        assert!(Request { gain: 0, ..Default::default() }.validate().is_err());
        assert!(Request { gain: 1024, ..Default::default() }.validate().is_err());
        assert!(Request::default().validate().is_ok());
    }
}
