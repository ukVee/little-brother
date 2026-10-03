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
//!
//! # The frames verb (protocol v2)
//!
//! `facial-id-defender`'s gate judges presence from IR frames, and the sensor
//! has one owner. So a root caller can ask for `count` frames and get them
//! back **on the socket** instead of on the loopback: same capture path, same
//! operating point, same teardown. Three rules shape it:
//!
//! - **Root callers only, for every request** — frames and the trigger alike
//!   — by `SO_PEERCRED` ([`may_use`]). The socket's file mode says the same,
//!   but a unit edit loosened it once (dotfiles `b869fc9`), so the daemon
//!   enforces it itself.
//! - **Auth always wins** ([`route`]). A frames request during an auth feed is
//!   refused `busy` without touching the sensor. An auth trigger during a
//!   frames capture ends that capture at once (its caller gets `preempted`)
//!   and starts the feed.
//! - **No frame reaches disk or the journal.** The journal gets counts and
//!   timings, and core dumps are off for the daemon ([`forbid_core_dumps`]).
//!
//! # Auto-exposure (the feed only)
//!
//! A feed runs [`ae`]'s loop on the frames it unpacks: in direct sun it
//! shortens the exposure until the centre stops clipping, and a room lit by
//! the emitter alone is never touched. The requested exposure is the
//! ceiling. A `frames` capture keeps exactly the exposure its caller asked
//! for: it is a measuring primitive, and its caller has the header `mean` to
//! run its own loop. `IRFACED_AUTO_EXPOSURE=0` turns the loop off.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use irctl::{subdev, video};
use tps68470::bus::Bus;
use tps68470::torch::{self, Torch};

use crate::ae::{self, Ae};
use crate::capture::{self, Graph};
use crate::faults;
use crate::signals;

pub const SOCKET_PATH: &str = "/run/cement-ir/irfaced.sock";
pub const REQ_MAGIC: u32 = 0x4652_4943; // "CIRF" little-endian
/// v1 is the trigger. A feed is still **sent** as v1, byte for byte, so the
/// PAM helper's request does not change: a warm daemon still running the
/// previous binary right after an install keeps serving auth.
pub const REQ_VERSION_FEED: u32 = 1;
/// v2 adds a request kind (byte 19) and a frame count (byte 20).
pub const REQ_VERSION: u32 = 2;
pub const REQ_BYTES: usize = 32;
pub const KIND_FEED: u8 = 0;
pub const KIND_FRAMES: u8 = 1;
pub const DEFAULT_FEED_SECS: u32 = 15;
pub const MAX_FEED_SECS: u32 = 120; // = the torch owner's duration ceiling
pub const DEFAULT_IDLE_SECS: u64 = 120;
pub const DEFAULT_GAIN: u16 = 128; // dlib scored best here (irface.py)
pub const DEFAULT_EXPOSURE: u32 = 1704;
pub const DEFAULT_STEPS: u8 = 1;
/// A presence poll asks for 3 frames; 10 is the ceiling.
pub const DEFAULT_FRAMES: u8 = 3;
pub const MAX_FRAMES: u8 = 10;
/// A frames capture that has not produced its count by now ends with an
/// error. The slowest first frame on record is 148 ms (the fid-gate probe-11
/// soak, 600 polls) and the sensor runs ~30 fps, so 10 frames take ~0.5 s:
/// this is the failure bound, not the expected time.
pub const FRAMES_DEADLINE_MS: u64 = 2000;
/// The most time the daemon spends writing one frames reply. The event loop
/// is blocked while it writes, so a client that does not read must not be
/// able to hold up an auth. A reading local client takes ~1 ms for 1 MB.
pub const SEND_DEADLINE_MS: u64 = 250;
/// One `frames` record header: `len u32 | width u16 | height u16 |
/// mono_ns u64 | mean f32 | darkness f32`, little-endian.
pub const FRAME_HEADER_BYTES: usize = 24;
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

// A frames capture never refreshes the torch MODE, so it must end well inside
// the watchdog that the refresh exists to beat.
const _: () = assert!(FRAMES_DEADLINE_MS < TORCH_REFRESH_SECS * 1000);

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Stream to the loopback for `duration_secs` — the auth path.
    Feed { duration_secs: u32 },
    /// Capture `count` frames and return them on the socket. Root only.
    Frames { count: u8 },
}

/// A request, fixed 32 bytes little-endian on the wire:
///
/// ```text
/// magic u32 | version u32 | duration_secs u32 | steps u8 | gain u16 |
/// exposure u32 | kind u8 (v2) | count u8 (v2) | reserved[11]
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub kind: Kind,
    pub current_steps: u8,
    pub gain: u16,
    pub exposure: u32,
}

impl Default for Request {
    fn default() -> Self {
        Request {
            kind: Kind::Feed { duration_secs: DEFAULT_FEED_SECS },
            current_steps: DEFAULT_STEPS,
            gain: DEFAULT_GAIN,
            exposure: DEFAULT_EXPOSURE,
        }
    }
}

impl Request {
    pub fn encode(&self) -> [u8; REQ_BYTES] {
        let (version, duration_secs, kind, count) = match self.kind {
            Kind::Feed { duration_secs } => (REQ_VERSION_FEED, duration_secs, KIND_FEED, 0),
            Kind::Frames { count } => (REQ_VERSION, 0, KIND_FRAMES, count),
        };
        let mut b = [0u8; REQ_BYTES];
        b[0..4].copy_from_slice(&REQ_MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&version.to_le_bytes());
        b[8..12].copy_from_slice(&duration_secs.to_le_bytes());
        b[12] = self.current_steps;
        b[13..15].copy_from_slice(&self.gain.to_le_bytes());
        b[15..19].copy_from_slice(&self.exposure.to_le_bytes());
        // A feed writes zeros here, which is exactly v1's reserved space.
        b[19] = kind;
        b[20] = count;
        // b[21..32] reserved, zero
        b
    }

    pub fn decode(buf: &[u8; REQ_BYTES]) -> Result<Request, String> {
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if magic != REQ_MAGIC {
            return Err(format!("bad magic 0x{magic:08x} (not a cement-ir trigger)"));
        }
        let duration_secs = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let kind = match version {
            // v1 predates the kind byte. Bytes 19..32 were reserved and never
            // checked, so they are not checked now either.
            REQ_VERSION_FEED => Kind::Feed { duration_secs },
            REQ_VERSION => {
                if buf[21..].iter().any(|&b| b != 0) {
                    return Err("reserved bytes 21..32 are not zero".to_string());
                }
                match (buf[19], buf[20]) {
                    (KIND_FEED, 0) => Kind::Feed { duration_secs },
                    (KIND_FEED, n) => {
                        return Err(format!("a feed request carries a frame count ({n})"))
                    }
                    (KIND_FRAMES, count) if duration_secs == 0 => Kind::Frames { count },
                    (KIND_FRAMES, _) => {
                        return Err(format!(
                            "a frames request carries a feed duration ({duration_secs}s)"
                        ))
                    }
                    (k, _) => return Err(format!("unknown request kind {k}")),
                }
            }
            v => return Err(format!("unsupported protocol version {v}")),
        };
        Ok(Request {
            kind,
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
        match self.kind {
            Kind::Feed { duration_secs } => {
                if duration_secs == 0 || duration_secs > MAX_FEED_SECS {
                    return Err(format!(
                        "duration {duration_secs}s is outside 1..={MAX_FEED_SECS} (the owner's own clock bounds every feed)"
                    ));
                }
            }
            Kind::Frames { count } => {
                if count == 0 || count > MAX_FRAMES {
                    return Err(format!("count {count} is outside 1..={MAX_FRAMES}"));
                }
            }
        }
        // 0 steps = no illuminator (legal — dark frames); 1..=3 go through
        // the owner's own ceiling check. >3 is refused by the owner before
        // any hardware is touched.
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

/// May this peer use the daemon at all? Root, or the daemon's own user.
///
/// The sensor is exclusive to its controllers (the owner's decision,
/// 2026-10-03): the PAM helper, root through `pam_exec seteuid` (an EUID is
/// what `SO_PEERCRED` reports), and `fid-gated`, root. Anything unprivileged
/// borrows through `fid-gated`, never this socket. A trigger lights the
/// emitter, and frames are face data, so neither is handed to a wheel process.
///
/// The production daemon runs as root, so that is uid 0 alone. A daemon a
/// developer runs as themselves on a private socket (`IRFACED_SOCKET`) serves
/// only themselves, which is what lets it be exercised without root.
pub fn may_use(peer_uid: u32, own_euid: u32) -> bool {
    peer_uid == 0 || peer_uid == own_euid
}

/// What the daemon is doing when a request arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Idle,
    Feeding,
    Grabbing,
}

/// Why a request was turned away before any hardware was touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Forbidden,
    BusyFeed,
    BusyGrab,
}

impl Refusal {
    fn code(self) -> &'static str {
        match self {
            Refusal::Forbidden => "forbidden",
            Refusal::BusyFeed | Refusal::BusyGrab => "busy",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Refusal::Forbidden => "irfaced serves root only",
            Refusal::BusyFeed => "an auth feed is live",
            Refusal::BusyGrab => "another frames capture is running",
        }
    }
}

/// What to do with a valid request, given what is running.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    StartFeed { duration_secs: u32 },
    /// A feed is already streaming: answer `ready`/`starting`, keep it.
    JoinFeed,
    /// End the frames capture now (its caller gets `preempted`), then start
    /// the feed.
    PreemptThenFeed { duration_secs: u32 },
    StartGrab { count: u8 },
    Refuse(Refusal),
}

/// The ownership rules, as one table, for a peer already allowed in
/// ([`may_use`] runs before the request is even read). Auth always wins: a
/// feed is never refused or queued behind a frames capture, and a frames
/// request never touches the sensor while a feed holds it.
fn route(kind: Kind, state: Busy) -> Route {
    match (kind, state) {
        (Kind::Feed { duration_secs }, Busy::Idle) => Route::StartFeed { duration_secs },
        (Kind::Feed { .. }, Busy::Feeding) => Route::JoinFeed,
        (Kind::Feed { duration_secs }, Busy::Grabbing) => Route::PreemptThenFeed { duration_secs },
        (Kind::Frames { .. }, Busy::Feeding) => Route::Refuse(Refusal::BusyFeed),
        (Kind::Frames { .. }, Busy::Grabbing) => Route::Refuse(Refusal::BusyGrab),
        (Kind::Frames { count }, Busy::Idle) => Route::StartGrab { count },
    }
}

/// The connecting process, as the kernel recorded it at `connect()`.
#[derive(Debug, Clone, Copy)]
struct Peer {
    uid: u32,
    pid: i32,
}

/// `SO_PEERCRED`: credentials the peer cannot forge.
fn peer_of(s: &UnixStream) -> io::Result<Peer> {
    // uid starts at u32::MAX, not 0, so a call that fills nothing can never
    // read as root.
    let mut cred = libc::ucred { pid: 0, uid: u32::MAX, gid: u32::MAX };
    let want = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let mut len = want;
    // SAFETY: a caller-owned ucred and its exact size; the kernel fills it.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    if len != want {
        return Err(io::Error::other(format!("SO_PEERCRED returned {len} bytes, want {want}")));
    }
    Ok(Peer { uid: cred.uid, pid: cred.pid })
}

fn who(peer: Option<Peer>) -> String {
    match peer {
        Some(p) => format!("uid {} pid {}", p.uid, p.pid),
        None => "an unidentified peer".to_string(),
    }
}

/// CLOCK_MONOTONIC now, in ns. `Instant` is the same clock but opaque, and a
/// frame's timestamp has to be comparable in the caller's process.
pub fn mono_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a caller-owned timespec; CLOCK_MONOTONIC always exists.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Mean level and raw darkness of an 8-bit frame, in one pass.
///
/// Darkness is the share of pixels in the darkest of 8 histogram bins over
/// 0..256 (values 0..=31), as a percentage: `howdy-compare`'s histogram, read
/// off the installed howdy-next 3.4 binary (`cv::calcHist` with histSize 8 and
/// range 0..256, then `hist[0] / sum * 100`), but on the frame as captured.
/// howdy applies CLAHE before it takes the histogram, so its number for the
/// same scene is lower (71.16 vs 92.1, garden fid-gate slice 004 Block 3), and
/// this value must not be compared with `dark_threshold`
/// (garden `projects/cement-ir/notes/003` §2).
pub fn stats(px: &[u8]) -> (f32, f32) {
    let (mut sum, mut dark) = (0u64, 0u64);
    for &p in px {
        sum += p as u64;
        if p < 32 {
            dark += 1;
        }
    }
    let n = px.len().max(1) as f64;
    ((sum as f64 / n) as f32, (dark as f64 * 100.0 / n) as f32)
}

/// What a caller needs to judge a frame without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMeta {
    /// CLOCK_MONOTONIC when the daemon dequeued the frame, in ns: at most one
    /// frame period after the exposure ended.
    pub mono_ns: u64,
    pub mean: f32,
    /// See [`stats`].
    pub darkness: f32,
}

/// One frame of a `frames` reply: 640x480 8-bit grey, the same `>> 2` the
/// loopback gets. Lives in memory only, and is dropped once the reply is out.
pub struct Frame {
    pub meta: FrameMeta,
    pub px: Vec<u8>,
}

impl Frame {
    pub fn new(mono_ns: u64, px: Vec<u8>) -> Frame {
        let (mean, darkness) = stats(&px);
        Frame { meta: FrameMeta { mono_ns, mean, darkness }, px }
    }
}

pub fn encode_frame_header(meta: &FrameMeta, len: usize) -> [u8; FRAME_HEADER_BYTES] {
    let mut b = [0u8; FRAME_HEADER_BYTES];
    b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    b[4..6].copy_from_slice(&(ip3y::WIDTH as u16).to_le_bytes());
    b[6..8].copy_from_slice(&(ip3y::HEIGHT as u16).to_le_bytes());
    b[8..16].copy_from_slice(&meta.mono_ns.to_le_bytes());
    b[16..20].copy_from_slice(&meta.mean.to_le_bytes());
    b[20..24].copy_from_slice(&meta.darkness.to_le_bytes());
    b
}

/// Decode a record header. Only a full 640x480 frame is accepted: the length
/// field is attacker-sized memory for whoever reads it, so it is checked
/// against the one size it can legitimately be before anything is allocated.
pub fn decode_frame_header(b: &[u8; FRAME_HEADER_BYTES]) -> Result<FrameMeta, String> {
    let len = u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize;
    let w = u16::from_le_bytes(b[4..6].try_into().unwrap()) as usize;
    let h = u16::from_le_bytes(b[6..8].try_into().unwrap()) as usize;
    if (w, h) != (ip3y::WIDTH, ip3y::HEIGHT) || len != ip3y::PIXELS {
        return Err(format!(
            "frame header says {w}x{h}, {len} bytes; want {}x{}",
            ip3y::WIDTH,
            ip3y::HEIGHT
        ));
    }
    Ok(FrameMeta {
        mono_ns: u64::from_le_bytes(b[8..16].try_into().unwrap()),
        mean: f32::from_le_bytes(b[16..20].try_into().unwrap()),
        darkness: f32::from_le_bytes(b[20..24].try_into().unwrap()),
    })
}

/// Read a `frames` reply: `frames <n>\n` then `n` records, or one
/// `error <code> (<detail>)` line, which comes back as `Err` verbatim.
pub fn read_frames(r: &mut impl BufRead) -> Result<Vec<Frame>, String> {
    let mut line = String::new();
    match r.by_ref().take(512).read_line(&mut line) {
        Ok(0) => return Err("closed without a reply".to_string()),
        Ok(_) => {}
        Err(e) => return Err(format!("no reply: {e}")),
    }
    let line = line.trim_end();
    let Some(n) = line.strip_prefix("frames ") else {
        return Err(line.to_string());
    };
    let n: usize = n.parse().map_err(|_| format!("bad reply header {line:?}"))?;
    if n == 0 || n > MAX_FRAMES as usize {
        return Err(format!("reply announces {n} frames (want 1..={MAX_FRAMES})"));
    }
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        let mut h = [0u8; FRAME_HEADER_BYTES];
        r.read_exact(&mut h).map_err(|e| format!("frame {i} header: {e}"))?;
        let meta = decode_frame_header(&h).map_err(|e| format!("frame {i}: {e}"))?;
        let mut px = vec![0u8; ip3y::PIXELS];
        r.read_exact(&mut px).map_err(|e| format!("frame {i} pixels: {e}"))?;
        frames.push(Frame { meta, px });
    }
    Ok(frames)
}

/// `write_all`, but the whole write is bounded by `deadline`, not each call.
fn write_all_by(s: &mut UnixStream, mut buf: &[u8], deadline: Instant) -> io::Result<()> {
    while !buf.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the client did not read in time"));
        }
        s.set_write_timeout(Some(left))?;
        match s.write(buf) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => buf = &buf[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn send_frames(s: &mut UnixStream, frames: &[Frame], deadline: Instant) -> io::Result<()> {
    write_all_by(s, format!("frames {}\n", frames.len()).as_bytes(), deadline)?;
    for f in frames {
        write_all_by(s, &encode_frame_header(&f.meta, f.px.len()), deadline)?;
        write_all_by(s, &f.px, deadline)?;
    }
    Ok(())
}

/// Whether a session is still running.
enum Progress {
    /// Frames still flowing (or waiting for the next one).
    Running,
    /// Deadline / signal / error / count reached: teardown done, reply sent.
    Done,
}

/// Every reply goes through `write`/`write_all`, never `write_vectored`:
/// std sends the former with `MSG_NOSIGNAL`, and `main` has put SIGPIPE back
/// to its default, so a vectored write to a client that hung up kills the
/// daemon (`tests/irfaced_daemon.rs`).
fn send_line(stream: &mut UnixStream, line: &str) {
    let _ = stream.write_all(line.as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

/// A coded refusal: the first word after `error` is what a client matches on.
fn refuse(stream: &mut UnixStream, code: &str, detail: &str) {
    send_line(stream, &format!("error {code} ({detail})"));
}

/// Bring the graph up, set the operating point, and STREAMON the capture —
/// the first half of every session, feed or frames. A failure carrying the
/// wedge signature is counted here, where the errno is still known.
fn open_capture(
    media_path: &Path,
    gain: u16,
    exposure: u32,
) -> Result<(Graph, video::Stream), String> {
    let graph = match capture::bring_up(media_path) {
        Ok(g) => g,
        Err(e) => {
            if e.wedged {
                // The rebind is a fault handler now (slice 007), so the
                // fault has to be visible: counted, and named in the
                // journal with the errno that identified it.
                faults::record(faults::WEDGE, &format!("bring-up: {}", e.msg));
            }
            return Err(e.msg);
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
    // the 2026-08-21 trap, garden hardware/notes/005).
    capture::set_operating_point("irfaced", &graph, gain, exposure);

    match video::Stream::start(graph.video.to_str().unwrap(), true, &video::ip3y_640x480(), NBUFS) {
        Ok(cap) => Ok((graph, cap)),
        Err(e) => {
            let msg = format!("capture {}: {e}", graph.video.display());
            if faults::looks_wedged(&e) {
                // STREAMON EINVAL after a torn-down stream: the signature
                // (garden hardware/notes/005). Counted, not silently
                // rebound. EBUSY is not the signature: it is another
                // client holding the sensor, and never arms anything.
                faults::record(faults::WEDGE, &msg);
            }
            Err(msg)
        }
    }
}

/// Light the torch through the one owner, in-process: the same guard RAII
/// (Drop clears WLEDCTL), the same GSTAT pre-check refusal, the same hard
/// caps — but no child process and no --parent plumbing, because THIS
/// process is the owner now. A refusal ends the session like cmd_feed's
/// sys.exit did (dark frames are worse than a password fallthrough with a
/// reason). `Ok(None)` at 0 steps: no illuminator was asked for.
fn light(steps: u8) -> Result<Option<Torch>, String> {
    if steps == 0 {
        return Ok(None);
    }
    Bus::open()
        .and_then(|bus| {
            // A latch left by an abnormal exit would otherwise refuse
            // every future auth forever (the pre-check runs before any
            // release, so nothing ever clears it). Clear a STALE one
            // here, once; a live fault returns immediately and the
            // owner's own pre-check below still refuses it.
            match torch::clear_stale_latch(&bus) {
                torch::StaleLatch::None => {}
                torch::StaleLatch::Cleared { was } => eprintln!(
                    "irfaced: cleared a stale GSTAT latch (0x{was:02x}) left by an earlier abnormal exit -- lighting anyway; a live fault would have come straight back"
                ),
                torch::StaleLatch::Live { gstat } => eprintln!(
                    "irfaced: GSTAT 0x{gstat:02x} survived a clear -- the fault is LIVE, refusing"
                ),
                torch::StaleLatch::Error(e) => {
                    eprintln!("irfaced: stale-latch check failed: {e}")
                }
            }
            Torch::on(bus, steps, 4).map_err(io::Error::other)
        })
        .map(Some)
        .map_err(|e| format!("torch refused: {e}"))
}

/// One in-flight feed: the torch guard, both streams, and the client socket
/// that gets the `ready` reply.
struct Feed {
    req: Request,
    duration_secs: u32,
    client: UnixStream,
    graph: Graph,
    cap: video::Stream,
    sink: video::Stream,
    torch: Option<Torch>,
    deadline: Instant,
    since_refresh: Instant,
    frames: u32,
    ready_sent: bool,
    /// The auto-exposure loop and the sensor subdev it writes, or `None` when
    /// it is off (by configuration, or after a failed write).
    ae: Option<(Ae, File)>,
}

/// Exposure changes journaled per feed; the rest are only counted.
const AE_LOG_CHANGES: u32 = 8;

/// One frame of the feed's auto-exposure loop: meter it, and write a new
/// exposure to the sensor when the loop asks for one. A write that fails
/// turns the loop off for the rest of the feed; the feed itself goes on at
/// whatever exposure the sensor last took.
fn auto_expose(ae: &mut Option<(Ae, File)>, grey: &[u8]) {
    let Some((a, sd)) = ae.as_mut() else { return };
    if a.settling() {
        return;
    }
    let from = a.exposure();
    let Some((to, why)) = a.step(ae::meter(grey, ip3y::WIDTH, ip3y::HEIGHT)) else {
        return;
    };
    match subdev::set_ctrl(sd, subdev::CID_EXPOSURE, to as i32) {
        Ok(()) if a.changes() <= AE_LOG_CHANGES => {
            eprintln!("irfaced: ae: exposure {from} -> {to} ({})", why.word())
        }
        Ok(()) => {}
        Err(e) => {
            eprintln!("irfaced: ae: set exposure={to} failed: {e}; fixed at {from} from here");
            *ae = None;
        }
    }
}

impl Feed {
    /// Build the whole capture session. On **any** failure the client gets a
    /// one-line `error <reason>` reply (which the helper logs; auth falls
    /// through to the password) and the same chain goes to the journal — the
    /// slice's "forced failure names the real reason" evidence.
    fn start(
        req: Request,
        duration_secs: u32,
        mut client: UnixStream,
        media_path: &Path,
        loopback: &str,
        auto_exposure: bool,
    ) -> Result<Feed, ()> {
        let fail = |client: &mut UnixStream, msg: &str| {
            send_line(client, &format!("error {msg}"));
            eprintln!("irfaced: cannot serve: {msg}");
        };
        eprintln!(
            "irfaced: request dur={}s steps={} gain={} exposure={}",
            duration_secs, req.current_steps, req.gain, req.exposure
        );
        let (graph, cap) = match open_capture(media_path, req.gain, req.exposure) {
            Ok(x) => x,
            Err(msg) => {
                fail(&mut client, &msg);
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
        let torch = match light(req.current_steps) {
            Ok(t) => t,
            Err(msg) => {
                fail(&mut client, &msg);
                return Err(());
            }
        };
        // A failure here costs the loop, never the feed: fixed exposure is
        // what every feed had before.
        let ae = match (auto_exposure, &graph.subdev) {
            (false, _) => None,
            (true, None) => {
                eprintln!("irfaced: ae: no sensor subdev; fixed exposure");
                None
            }
            (true, Some(sd)) => match File::open(sd) {
                Ok(f) => Some((Ae::new(req.exposure), f)),
                Err(e) => {
                    eprintln!("irfaced: ae: cannot open {}: {e}; fixed exposure", sd.display());
                    None
                }
            },
        };

        Ok(Feed {
            req,
            duration_secs,
            client,
            graph,
            cap,
            sink,
            torch,
            deadline: Instant::now() + Duration::from_secs(duration_secs as u64),
            since_refresh: Instant::now(),
            frames: 0,
            ready_sent: false,
            ae,
        })
    }

    /// Pump one tick of the feed. Bounded work; the caller drives it from the
    /// event loop so new connections still get accepted mid-feed.
    fn drive(&mut self) -> Progress {
        if signals::SIGNALLED.load(Ordering::SeqCst) || Instant::now() >= self.deadline {
            self.finish();
            return Progress::Done;
        }
        // Defence in depth: the frame-count bound cmd_feed sized by the SENSOR
        // rate — a flooding sensor ends the feed instead of spinning.
        if self.frames >= self.duration_secs * SENSOR_FPS + 50 {
            eprintln!("irfaced: frame bound reached ({} frames); ending the feed", self.frames);
            self.finish();
            return Progress::Done;
        }
        // Refresh the torch MODE inside the ~13 s hardware watchdog.
        if let Some(t) = &self.torch {
            if self.since_refresh.elapsed() >= Duration::from_secs(TORCH_REFRESH_SECS) {
                if let Err(e) = t.refresh() {
                    eprintln!("irfaced: MODE refresh failed: {e}; ending the feed");
                    self.finish();
                    return Progress::Done;
                }
                self.since_refresh = Instant::now();
            }
        }
        // One capture -> unpack -> loopback hop, if a frame is ready.
        let Self { cap, sink, client, frames, ready_sent, graph, ae, .. } = self;
        let result = cap.with_frame(TICK_MS, |frame| {
            let mut px = vec![0u16; ip3y::PIXELS];
            ip3y::unpack(frame, &mut px);
            // 10-bit -> 8-bit (the >>2 the Python feeder used).
            let grey: Vec<u8> = px.iter().map(|v| (v >> 2) as u8).collect();
            auto_expose(ae, &grey);
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
            Ok(_) => Progress::Running,
            Err(e) => {
                let msg = format!("feed error after {} frames: {e}", *frames);
                eprintln!("irfaced: {msg}");
                if faults::looks_wedged(&e) {
                    faults::record(faults::WEDGE, &msg);
                }
                self.finish();
                Progress::Done
            }
        }
    }

    /// Teardown in the learned order — `capture::teardown`, the one copy the
    /// soak also drives (slice 007). Never returns an error: every step
    /// reports its own, and the feed is over either way.
    fn finish(&mut self) {
        let torch = self.torch.take();
        let t = capture::teardown("irfaced", torch, &mut self.cap, Some(&mut self.sink), &self.graph);
        eprintln!(
            "irfaced: fed {} frames from {} ({} -> {}); {}; dur={}s steps={} ae={}",
            self.frames,
            self.graph.video.display(),
            self.graph.sensor,
            self.graph.csi2,
            t.readback.describe(),
            self.duration_secs,
            self.req.current_steps,
            match &self.ae {
                Some((a, _)) => format!("{} change(s)", a.changes()),
                None => "off".into(),
            }
        );
        // A feed that streamed nothing, or one whose teardown did not come
        // apart cleanly, is the wedge showing up on the auth path itself. The
        // rebind no longer runs here (that is the slice), so the fault rate is
        // the evidence: `cement-ir doctor` reports it.
        if self.frames == 0 {
            faults::record(
                faults::WEDGE,
                &format!("fed 0 frames in {}s", self.duration_secs),
            );
        }
        if !t.errors.is_empty() {
            faults::record(faults::WEDGE, &format!("teardown: {}", t.errors.join("; ")));
        }
    }
}

/// How a frames capture ended. Decides the reply and, separately, whether the
/// fault counter hears about it.
enum End {
    Complete,
    /// An auth trigger took the sensor.
    Preempted,
    /// The daemon is exiting.
    Stopping,
    Deadline,
    Failed { msg: String, wedged: bool },
}

/// One in-flight `frames` capture. Driven tick by tick from the event loop,
/// like a feed, so an auth trigger is accepted (and wins) mid-capture.
struct Grab {
    want: u8,
    steps: u8,
    client: UnixStream,
    peer: Option<Peer>,
    graph: Graph,
    cap: video::Stream,
    torch: Option<Torch>,
    started: Instant,
    deadline: Instant,
    frames: Vec<Frame>,
}

impl Grab {
    /// Capture side only: no loopback sink, so `pam_howdy` never sees these
    /// frames and they never leave this process except on the socket.
    fn start(
        want: u8,
        req: Request,
        mut client: UnixStream,
        peer: Option<Peer>,
        media_path: &Path,
    ) -> Result<Grab, ()> {
        let started = Instant::now();
        eprintln!(
            "irfaced: frames request count={want} steps={} gain={} exposure={} from {}",
            req.current_steps,
            req.gain,
            req.exposure,
            who(peer)
        );
        let fail = |client: &mut UnixStream, code: &str, msg: &str| {
            refuse(client, code, msg);
            eprintln!("irfaced: frames: cannot serve: {msg}");
        };
        let (graph, cap) = match open_capture(media_path, req.gain, req.exposure) {
            Ok(x) => x,
            Err(msg) => {
                fail(&mut client, "capture", &msg);
                return Err(());
            }
        };
        let torch = match light(req.current_steps) {
            Ok(t) => t,
            Err(msg) => {
                fail(&mut client, "torch", &msg);
                return Err(());
            }
        };
        Ok(Grab {
            want,
            steps: req.current_steps,
            client,
            peer,
            graph,
            cap,
            torch,
            started,
            deadline: started + Duration::from_millis(FRAMES_DEADLINE_MS),
            frames: Vec::with_capacity(want as usize),
        })
    }

    fn drive(&mut self) -> Progress {
        if signals::SIGNALLED.load(Ordering::SeqCst) {
            self.finish(End::Stopping);
            return Progress::Done;
        }
        if Instant::now() >= self.deadline {
            self.finish(End::Deadline);
            return Progress::Done;
        }
        let frames = &mut self.frames;
        let result = self.cap.with_frame(TICK_MS, |raw| {
            let mut px = vec![0u16; ip3y::PIXELS];
            ip3y::unpack(raw, &mut px);
            // 10-bit -> 8-bit, exactly what the loopback gets.
            frames.push(Frame::new(mono_ns(), px.iter().map(|v| (v >> 2) as u8).collect()));
            Ok(())
        });
        match result {
            Ok(_) if self.frames.len() >= self.want as usize => {
                self.finish(End::Complete);
                Progress::Done
            }
            Ok(_) => Progress::Running,
            Err(e) => {
                let msg = format!("capture error after {} frames: {e}", self.frames.len());
                self.finish(End::Failed { wedged: faults::looks_wedged(&e), msg });
                Progress::Done
            }
        }
    }

    /// Teardown first (torch out, then STREAMOFF), then the reply: the
    /// hardware is released before a slow client can hold anything up.
    fn finish(&mut self, end: End) {
        let torch = self.torch.take();
        let t = capture::teardown("irfaced", torch, &mut self.cap, None, &self.graph);
        let got = self.frames.len();
        let (code, detail) = match &end {
            End::Complete => ("", String::new()),
            End::Preempted => ("preempted", "an auth feed took the sensor".to_string()),
            End::Stopping => ("stopping", "the daemon is exiting".to_string()),
            End::Deadline if got == 0 => {
                ("capture", format!("no frame within {FRAMES_DEADLINE_MS} ms"))
            }
            End::Deadline => (
                "short",
                format!("{got} of {} frames within {FRAMES_DEADLINE_MS} ms", self.want),
            ),
            End::Failed { msg, .. } => ("capture", msg.clone()),
        };
        // Counts and timings only. Never a pixel, and never a frame
        // statistic either: the caller gets those, the journal does not.
        eprintln!(
            "irfaced: frames: {got}/{} for {} in {} ms ({}); {}; steps={}",
            self.want,
            who(self.peer),
            self.started.elapsed().as_millis(),
            if code.is_empty() { "complete" } else { code },
            t.readback.describe(),
            self.steps
        );
        // The a8224c2 rule: a shortfall is a wedge only when nothing else
        // explains it. A preempted or stopped capture is explained, however
        // few frames it got; so is any capture that streamed at all.
        match &end {
            End::Failed { msg, wedged: true } => {
                faults::record(faults::WEDGE, &format!("frames: {msg}"))
            }
            End::Deadline if got == 0 => faults::record(
                faults::WEDGE,
                &format!("frames: 0 frames in {FRAMES_DEADLINE_MS} ms"),
            ),
            _ => {}
        }
        if !t.errors.is_empty() {
            faults::record(faults::WEDGE, &format!("teardown: {}", t.errors.join("; ")));
        }
        let frames = std::mem::take(&mut self.frames);
        if let End::Complete = end {
            let deadline = Instant::now() + Duration::from_millis(SEND_DEADLINE_MS);
            if let Err(e) = send_frames(&mut self.client, &frames, deadline) {
                eprintln!("irfaced: frames: reply to {} not delivered: {e}", who(self.peer));
            }
        } else {
            refuse(&mut self.client, code, &detail);
        }
    }
}

/// Whatever holds the sensor right now.
enum Active {
    Feed(Feed),
    Grab(Grab),
}

impl Active {
    fn state(&self) -> Busy {
        match self {
            Active::Feed(_) => Busy::Feeding,
            Active::Grab(_) => Busy::Grabbing,
        }
    }

    fn drive(&mut self) -> Progress {
        match self {
            Active::Feed(f) => f.drive(),
            Active::Grab(g) => g.drive(),
        }
    }

    /// The signal path: release everything before exiting.
    fn stop(&mut self) {
        match self {
            Active::Feed(f) => f.finish(),
            Active::Grab(g) => g.finish(End::Stopping),
        }
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

/// What every connection is served against.
struct Ctx {
    media_path: PathBuf,
    loopback: String,
    /// The daemon's own euid, for [`may_use`].
    own_euid: u32,
    /// Whether feeds run the auto-exposure loop (`IRFACED_AUTO_EXPOSURE`).
    auto_exposure: bool,
}

fn start_feed(
    req: Request,
    duration_secs: u32,
    stream: UnixStream,
    active: &mut Option<Active>,
    ctx: &Ctx,
) {
    // Feed::start replies "error <reason>" on the socket itself on any
    // failure; on success the socket is the feed's ready-reply channel.
    if let Ok(f) = Feed::start(
        req,
        duration_secs,
        stream,
        &ctx.media_path,
        &ctx.loopback,
        ctx.auto_exposure,
    ) {
        *active = Some(Active::Feed(f));
    }
}

/// Handle one connection: who is it, then parse + validate, then do what
/// [`route`] says.
fn handle_connection(stream: UnixStream, active: &mut Option<Active>, ctx: &Ctx) {
    let mut stream = stream;
    // Who, before a single byte is read: a peer that may not use the daemon
    // is refused at once, so it cannot even hold this single-threaded loop for
    // the read timeout below while an auth waits. A peer that cannot be
    // identified is refused too (fail closed).
    let peer = match peer_of(&stream) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("irfaced: SO_PEERCRED failed: {e}");
            None
        }
    };
    if !peer.is_some_and(|p| may_use(p.uid, ctx.own_euid)) {
        let r = Refusal::Forbidden;
        refuse(&mut stream, r.code(), r.detail());
        eprintln!("irfaced: refused {}: {} ({})", who(peer), r.code(), r.detail());
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; REQ_BYTES];
    if let Err(e) = stream.read_exact(&mut buf) {
        eprintln!("irfaced: dropped a connection: {e}");
        return;
    }
    let req = match Request::decode(&buf).and_then(|r| r.validate().map(|_| r)) {
        Ok(r) => r,
        Err(msg) => {
            refuse(&mut stream, "invalid", &msg);
            eprintln!("irfaced: refused a request: {msg}");
            return;
        }
    };
    let state = active.as_ref().map_or(Busy::Idle, Active::state);
    match route(req.kind, state) {
        Route::StartFeed { duration_secs } => start_feed(req, duration_secs, stream, active, ctx),
        Route::JoinFeed => {
            // Another auth attempt while the feed runs: the frames are already
            // flowing, which is exactly what the helper's old "feeder already
            // running -> exit 0" short-circuit meant. Do not restart the feed.
            if let Some(Active::Feed(f)) = active {
                send_line(&mut stream, if f.ready_sent { "ready" } else { "starting" });
            }
        }
        Route::PreemptThenFeed { duration_secs } => {
            let t0 = Instant::now();
            if let Some(Active::Grab(mut g)) = active.take() {
                g.finish(End::Preempted);
            }
            eprintln!(
                "irfaced: an auth trigger preempted a frames capture ({} ms to release the sensor)",
                t0.elapsed().as_millis()
            );
            start_feed(req, duration_secs, stream, active, ctx);
        }
        Route::StartGrab { count } => {
            if let Ok(g) = Grab::start(count, req, stream, peer, &ctx.media_path) {
                *active = Some(Active::Grab(g));
            }
        }
        Route::Refuse(r) => {
            refuse(&mut stream, r.code(), r.detail());
            eprintln!("irfaced: refused frames to {}: {} ({})", who(peer), r.code(), r.detail());
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Frames are biometric data and must never reach disk. The one deterministic
/// route there is a core dump: this machine's `core_pattern` is
/// `systemd-coredump`, and a panic while a frames reply sits in memory would
/// store it under `/var/lib/systemd/coredump`. `systemd-coredump` honours a
/// zero `RLIMIT_CORE` and stores nothing. `PR_SET_DUMPABLE` would not do: with
/// `fs.suid_dumpable = 2`, which systemd sets, a non-dumpable process is still
/// dumped, as root.
fn forbid_core_dumps() {
    let zero = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: a plain struct passed by reference.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) } != 0 {
        eprintln!(
            "irfaced: WARNING: could not disable core dumps: {}",
            io::Error::last_os_error()
        );
    }
}

/// `cement-ir-rt irfaced daemon` — run the socket-activated service.
pub fn daemon_run() -> std::process::ExitCode {
    forbid_core_dumps();
    let ctx = Ctx {
        media_path: PathBuf::from(env_or("IRFACED_MEDIA", "/dev/media0")),
        loopback: env_or("IRFACED_LOOPBACK", "/dev/video20"),
        // SAFETY: geteuid() is always safe; the wrapper just isn't marked.
        own_euid: unsafe { libc::geteuid() },
        auto_exposure: !matches!(
            env_or("IRFACED_AUTO_EXPOSURE", "1").as_str(),
            "0" | "off" | "no" | "false"
        ),
    };
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
        "irfaced: media={} loopback={} idle_timeout={idle_secs}s auto_exposure={}",
        ctx.media_path.display(),
        ctx.loopback,
        if ctx.auto_exposure { "on" } else { "off" }
    );

    let mut active: Option<Active> = None;
    let mut last_activity = Instant::now();
    loop {
        if signals::SIGNALLED.load(Ordering::SeqCst) {
            eprintln!("irfaced: signal received; releasing and exiting");
            break;
        }
        // Drive the active session first (short, bounded), then accept.
        if let Some(a) = active.as_mut() {
            if let Progress::Done = a.drive() {
                active = None;
                last_activity = Instant::now();
            }
        }
        // With a session running, `drive` above already paced this loop — it
        // blocks up to TICK_MS inside `with_frame`. With NONE there is
        // nothing to block on, and a non-blocking accept() in a bare loop is a
        // busy-wait: the first deployed daemon burned 1 min 50 s of CPU over a
        // 2 min 5 s life serving nothing (2026-09-26, D3). On a fanless device
        // with a thermal-shutdown history that is not a rounding error, and it
        // falsifies the "at rest nothing runs" claim slice 011's power budget
        // is built on. So wait on the listener instead of asking it repeatedly.
        if active.is_none() {
            wait_readable(&listener, idle_secs, last_activity);
        }
        match listener.accept() {
            Ok((stream, _)) => {
                last_activity = Instant::now();
                handle_connection(stream, &mut active, &ctx);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => eprintln!("irfaced: accept: {e}"),
        }
        if active.is_none() && last_activity.elapsed() >= Duration::from_secs(idle_secs) {
            eprintln!(
                "irfaced: idle for {idle_secs}s; exiting (the socket unit reactivates on the next connect)"
            );
            break;
        }
    }
    if let Some(mut a) = active.take() {
        a.stop();
    }
    std::process::ExitCode::SUCCESS
}

/// Block until the listener has a connection waiting or the idle deadline
/// passes, whichever comes first — the idle path's pacing.
///
/// The timeout is the time left on the idle budget, so the loop still exits on
/// schedule without polling for it: a daemon with 118 s left sleeps 118 s, not
/// 118 s worth of `accept()` calls. `poll` returning `EINTR` is normal here (a
/// signal arrived) and needs no handling — the caller re-checks `SIGNALLED` at
/// the top of the next pass.
fn wait_readable(listener: &UnixListener, idle_secs: u64, last_activity: Instant) {
    let left = Duration::from_secs(idle_secs).saturating_sub(last_activity.elapsed());
    if left.is_zero() {
        return; // the caller's own idle check is about to fire
    }
    let timeout_ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
    let mut pfd = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialised pollfd, count 1, and a plain timeout.
    unsafe { libc::poll(&raw mut pfd, 1, timeout_ms) };
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
    let req = Request { kind: Kind::Feed { duration_secs }, current_steps: steps, gain, exposure };
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

/// The `cement-ir-rt irfaced frames` client — the verb's test surface, and
/// the reference reader for `fid-gated`. Prints each frame's metadata and
/// drops the pixels: nothing is written anywhere. Unlike `trigger`, it exits
/// non-zero on any error reply, because nothing waits on it.
pub fn frames_run(
    count: u8,
    steps: u8,
    gain: u16,
    exposure: u32,
    timeout_secs: u64,
    socket: &str,
) -> std::process::ExitCode {
    let req = Request { kind: Kind::Frames { count }, current_steps: steps, gain, exposure };
    if let Err(msg) = req.validate() {
        println!("frames: {msg}");
        return std::process::ExitCode::FAILURE;
    }
    let started = Instant::now();
    let mut stream = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(e) => {
            println!("frames: unreachable: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Err(e) = stream.write_all(&req.encode()) {
        println!("frames: send failed: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(timeout_secs)));
    let result = read_frames(&mut BufReader::new(stream));
    let ms = started.elapsed().as_millis();
    let frames = match result {
        Ok(f) => f,
        Err(reply) => {
            println!("frames: {reply} ({ms} ms)");
            return std::process::ExitCode::FAILURE;
        }
    };
    let now = mono_ns();
    let first = frames[0].meta.mono_ns;
    let mut bad = false;
    for (i, f) in frames.iter().enumerate() {
        // Recompute from the pixels: proves the transfer and the header agree.
        let (mean, darkness) = stats(&f.px);
        let agree = (mean - f.meta.mean).abs() < 0.01 && (darkness - f.meta.darkness).abs() < 0.01;
        bad |= !agree;
        println!(
            "frame {}: +{:.1} ms  mean {:.1}  darkness {:.2}%{}",
            i + 1,
            f.meta.mono_ns.saturating_sub(first) as f64 / 1e6,
            f.meta.mean,
            f.meta.darkness,
            if agree { "" } else { "  (header disagrees with the pixels!)" }
        );
    }
    let age_ms = now.saturating_sub(frames[frames.len() - 1].meta.mono_ns) as f64 / 1e6;
    println!(
        "frames: {} in {ms} ms since connect; the last frame was {age_ms:.1} ms old on receipt",
        frames.len()
    );
    if bad {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

/// `cement-ir-rt irfaced resolve` — resolve + configure the graph, print the
/// result, do not stream. The verification surface for the by-name resolution
/// (mirrors `cement-ir up`'s output).
pub fn resolve_run(media_path: &str) -> std::process::ExitCode {
    match capture::bring_up(Path::new(media_path)) {
        Ok(g) => {
            println!("sensor: {}  ->  {}", g.sensor, g.csi2);
            println!("ready: {} (ip3y 640x480)", g.video.display());
            match &g.subdev {
                Some(sd) => println!("sensor ctrls node: {}", sd.display()),
                None => println!("WARNING: no subdev node for gain/exposure"),
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cement-ir-rt: {}", e.msg);
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_req(count: u8) -> Request {
        Request { kind: Kind::Frames { count }, ..Default::default() }
    }

    #[test]
    fn request_roundtrip() {
        let r = Request { current_steps: 2, gain: 200, exposure: 900, ..Default::default() };
        let buf = r.encode();
        assert_eq!(buf.len(), REQ_BYTES);
        assert_eq!(Request::decode(&buf).unwrap(), r);
        let f = Request { current_steps: 2, gain: 200, exposure: 900, ..frames_req(3) };
        assert_eq!(Request::decode(&f.encode()).unwrap(), f);
    }

    /// The auth path's bytes must not change: the PAM helper's request is
    /// still the v1 layout, so a stale warm daemon keeps serving it.
    #[test]
    fn a_feed_is_still_sent_as_v1_byte_for_byte() {
        let buf = Request::default().encode();
        let mut v1 = [0u8; REQ_BYTES];
        v1[0..4].copy_from_slice(&REQ_MAGIC.to_le_bytes());
        v1[4..8].copy_from_slice(&1u32.to_le_bytes());
        v1[8..12].copy_from_slice(&15u32.to_le_bytes());
        v1[12] = 1;
        v1[13..15].copy_from_slice(&128u16.to_le_bytes());
        v1[15..19].copy_from_slice(&1704u32.to_le_bytes());
        assert_eq!(buf, v1);
        // and v1 never checked its reserved bytes, so neither does decode
        let mut junk = v1;
        junk[19] = 1;
        junk[25] = 0xff;
        assert_eq!(Request::decode(&junk).unwrap().kind, Kind::Feed { duration_secs: 15 });
    }

    #[test]
    fn frames_go_out_as_v2() {
        let buf = frames_req(3).encode();
        assert_eq!(u32::from_le_bytes(buf[4..8].try_into().unwrap()), 2);
        assert_eq!((buf[19], buf[20]), (KIND_FRAMES, 3));
        assert_eq!(&buf[8..12], &[0; 4], "no feed duration on a frames request");
    }

    #[test]
    fn v2_is_strict() {
        let good = frames_req(3).encode();
        let mut b = good;
        b[31] = 1;
        assert!(Request::decode(&b).unwrap_err().contains("reserved"));
        let mut b = good;
        b[19] = 7;
        assert!(Request::decode(&b).unwrap_err().contains("kind"));
        let mut b = good;
        b[8] = 5;
        assert!(Request::decode(&b).unwrap_err().contains("duration"));
        // a v2 feed is legal, but not one carrying a count
        let mut b = good;
        b[19] = KIND_FEED;
        b[20] = 0;
        b[8] = 15;
        assert_eq!(Request::decode(&b).unwrap().kind, Kind::Feed { duration_secs: 15 });
        b[20] = 2;
        assert!(Request::decode(&b).unwrap_err().contains("count"));
    }

    #[test]
    fn request_rejects_bad_magic_and_version() {
        let mut buf = Request::default().encode();
        buf[0] ^= 0xff;
        assert!(Request::decode(&buf).unwrap_err().contains("magic"));
        let mut buf = Request::default().encode();
        buf[7] = 99;
        assert!(Request::decode(&buf).unwrap_err().contains("version"));
        let mut buf = Request::default().encode();
        buf[4] = 3;
        assert!(Request::decode(&buf).unwrap_err().contains("version 3"));
    }

    #[test]
    fn policy_checks_precede_hardware() {
        // Duration outside 1..=120, steps above the owner's ceiling, gain 0:
        // all refused before any device is touched.
        let feed = |d| Request { kind: Kind::Feed { duration_secs: d }, ..Default::default() };
        assert!(feed(0).validate().is_err());
        assert!(feed(121).validate().is_err());
        assert!(Request { current_steps: 4, ..Default::default() }.validate().is_err());
        assert!(Request { current_steps: 0, ..Default::default() }.validate().is_ok());
        assert!(Request { gain: 0, ..Default::default() }.validate().is_err());
        assert!(Request { gain: 1024, ..Default::default() }.validate().is_err());
        assert!(Request::default().validate().is_ok());
        // frames: 1..=10, and the same steps/gain rules
        assert!(frames_req(0).validate().is_err());
        assert!(frames_req(11).validate().is_err());
        assert!(frames_req(1).validate().is_ok());
        assert!(frames_req(MAX_FRAMES).validate().is_ok());
        assert!(Request { current_steps: 4, ..frames_req(3) }.validate().is_err());
    }

    #[test]
    fn only_root_or_the_daemons_own_user_is_served() {
        assert!(may_use(0, 0), "root, to the root daemon");
        assert!(!may_use(1000, 0), "ukv, to the root daemon: forbidden");
        assert!(!may_use(65534, 0));
        assert!(may_use(1000, 1000), "a developer's own daemon");
        assert!(may_use(0, 1000), "root, to a developer's daemon");
        assert!(!may_use(1001, 1000));
    }

    /// Every (kind, state, allowed) combination, against the rules.
    #[test]
    fn the_routing_table() {
        use Busy::*;
        let feed = Kind::Feed { duration_secs: 15 };
        let frames = Kind::Frames { count: 3 };
        // a feed always proceeds
        assert_eq!(route(feed, Idle), Route::StartFeed { duration_secs: 15 });
        assert_eq!(route(feed, Feeding), Route::JoinFeed);
        assert_eq!(route(feed, Grabbing), Route::PreemptThenFeed { duration_secs: 15 });
        // frames yield to anything already running
        assert_eq!(route(frames, Idle), Route::StartGrab { count: 3 });
        assert_eq!(route(frames, Feeding), Route::Refuse(Refusal::BusyFeed));
        assert_eq!(route(frames, Grabbing), Route::Refuse(Refusal::BusyGrab));
        // forbidden is decided before the request is read, never here
        for k in [feed, frames] {
            for s in [Idle, Feeding, Grabbing] {
                assert_ne!(route(k, s), Route::Refuse(Refusal::Forbidden));
            }
        }
    }

    #[test]
    fn darkness_is_the_raw_first_bin_share() {
        assert_eq!(stats(&[0; 100]), (0.0, 100.0));
        assert_eq!(stats(&[255; 100]), (255.0, 0.0));
        // the bin edge: 31 is in the darkest of 8 bins over 0..256, 32 is not
        assert_eq!(stats(&[31, 32]).1, 50.0);
        let px: Vec<u8> = (0..=255).collect();
        let (mean, dark) = stats(&px);
        assert_eq!(mean, 127.5);
        assert_eq!(dark, 12.5, "32 of 256 values");
        // a full-size frame stays exact (sum overflows f32's mantissa)
        let big = vec![200u8; ip3y::PIXELS];
        assert_eq!(stats(&big), (200.0, 0.0));
    }

    #[test]
    fn frame_header_roundtrip_and_size_check() {
        let m = FrameMeta { mono_ns: 123_456_789_012, mean: 41.25, darkness: 76.9 };
        let h = encode_frame_header(&m, ip3y::PIXELS);
        assert_eq!(decode_frame_header(&h).unwrap(), m);
        let bad = encode_frame_header(&m, ip3y::PIXELS + 1);
        assert!(decode_frame_header(&bad).is_err(), "a length that is not one frame");
        let mut bad = h;
        bad[4] = 0;
        assert!(decode_frame_header(&bad).is_err(), "a width that is not 640");
    }

    fn synthetic(n: usize) -> Vec<Frame> {
        // Synthetic fixtures only: never a real frame in a test.
        (0..n)
            .map(|i| Frame::new(1_000_000 * i as u64, vec![(i * 40) as u8; ip3y::PIXELS]))
            .collect()
    }

    #[test]
    fn a_reply_reads_back_as_sent() {
        let (mut tx, rx) = UnixStream::pair().unwrap();
        let sent = synthetic(3);
        let metas: Vec<FrameMeta> = sent.iter().map(|f| f.meta).collect();
        let reader = std::thread::spawn(move || read_frames(&mut BufReader::new(rx)));
        send_frames(&mut tx, &sent, Instant::now() + Duration::from_secs(5)).unwrap();
        let got = reader.join().unwrap().unwrap();
        assert_eq!(got.iter().map(|f| f.meta).collect::<Vec<_>>(), metas);
        assert!(got.iter().zip(&sent).all(|(a, b)| a.px == b.px));
    }

    #[test]
    fn an_error_reply_comes_back_verbatim() {
        let (mut tx, rx) = UnixStream::pair().unwrap();
        refuse(&mut tx, "busy", "an auth feed is live");
        let e = read_frames(&mut BufReader::new(rx)).err().unwrap();
        assert_eq!(e, "error busy (an auth feed is live)");
    }

    /// A client that never reads cannot hold the event loop past the send
    /// deadline (3 frames are ~900 KB, several socket buffers).
    #[test]
    fn a_client_that_does_not_read_is_dropped_at_the_deadline() {
        let (mut tx, _rx) = UnixStream::pair().unwrap();
        let t0 = Instant::now();
        let e = send_frames(&mut tx, &synthetic(3), t0 + Duration::from_millis(100)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t0.elapsed() < Duration::from_millis(500), "took {:?}", t0.elapsed());
    }

    #[test]
    fn peer_cred_names_the_connecting_process() {
        let (a, _b) = UnixStream::pair().unwrap();
        let p = peer_of(&a).unwrap();
        assert_eq!(p.uid, unsafe { libc::geteuid() });
        assert_eq!(p.pid, std::process::id() as i32);
    }

    /// Serve one request through the real `handle_connection` over a socket
    /// pair, and return the reply line.
    fn serve(req: &[u8; REQ_BYTES], ctx: &Ctx) -> (String, bool) {
        let (mut client, server) = UnixStream::pair().unwrap();
        client.write_all(req).unwrap();
        let mut active = None;
        handle_connection(server, &mut active, ctx);
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        (line.trim_end().to_string(), active.is_some())
    }

    fn ctx(own_euid: u32) -> Ctx {
        Ctx {
            // nothing here: an allowed request must fail at bring-up, never
            // reach a real sensor from a unit test
            media_path: PathBuf::from("/nonexistent/cement-ir-test-media"),
            loopback: "/nonexistent/cement-ir-test-loopback".to_string(),
            own_euid,
            auto_exposure: true,
        }
    }

    #[test]
    fn a_non_root_peer_is_refused_anything_before_any_hardware() {
        let me = unsafe { libc::geteuid() };
        if me == 0 {
            return; // the check is about a non-root peer; root passes it
        }
        // Pretend to be the production (root) daemon: this test process is
        // then a non-root peer. The trigger is refused like frames, and so is
        // garbage: the peer is judged before a byte of the request is read.
        let mut garbage = Request::default().encode();
        garbage[0] = 0;
        for req in [frames_req(3).encode(), Request::default().encode(), garbage] {
            let (reply, holds) = serve(&req, &ctx(0));
            assert_eq!(reply, "error forbidden (irfaced serves root only)");
            assert!(!holds);
        }
    }

    /// A refused peer that connects and never writes is answered at once: it
    /// cannot hold the single-threaded loop for the read timeout.
    #[test]
    fn a_silent_non_root_peer_cannot_stall_the_loop() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (client, server) = UnixStream::pair().unwrap();
        let t0 = Instant::now();
        let mut active = None;
        handle_connection(server, &mut active, &ctx(0));
        assert!(t0.elapsed() < Duration::from_millis(500), "took {:?}", t0.elapsed());
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        assert!(line.starts_with("error forbidden"), "{line}");
    }

    #[test]
    fn an_allowed_peer_reaches_the_capture_stage() {
        let me = unsafe { libc::geteuid() };
        let (reply, holds) = serve(&frames_req(3).encode(), &ctx(me));
        assert!(reply.starts_with("error capture (cannot open /nonexistent/"), "{reply}");
        assert!(!holds);
    }

    #[test]
    fn an_invalid_request_is_refused_as_invalid() {
        let me = unsafe { libc::geteuid() };
        let (reply, _) = serve(&frames_req(11).encode(), &ctx(me));
        assert_eq!(reply, "error invalid (count 11 is outside 1..=10)");
        let mut bad = Request::default().encode();
        bad[0] = 0;
        let (reply, _) = serve(&bad, &ctx(me));
        assert!(reply.starts_with("error invalid (bad magic"), "{reply}");
    }
}
