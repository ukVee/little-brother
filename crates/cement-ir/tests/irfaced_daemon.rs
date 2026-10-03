//! The daemon as a real process on a private socket. Nothing here reaches the
//! sensor: `IRFACED_MEDIA` points at nothing, so every request fails at
//! bring-up and is answered with one `error` line.
//!
//! The requests are built by hand, not with the binary's own encoder, so this
//! also pins the wire layout from the outside.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn spawn(name: &str) -> Daemon {
    let dir = std::env::temp_dir().join(format!("irfaced-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("irfaced.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_cement-ir-rt"))
        .args(["irfaced", "daemon"])
        .env("IRFACED_SOCKET", &socket)
        .env("IRFACED_MEDIA", dir.join("no-media"))
        .env("IRFACED_LOOPBACK", dir.join("no-loopback"))
        .env("IRFACED_IDLE_SECS", "30")
        .env("CEMENT_IR_FAULTS", dir.join("faults"))
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while !socket.exists() {
        assert!(t0.elapsed() < Duration::from_secs(5), "the daemon never bound {socket:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    Daemon { child, socket, dir }
}

/// `magic | version | duration | steps | gain | exposure | kind | count | 0..`
fn request(version: u32, duration: u32, kind: u8, count: u8) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[0..4].copy_from_slice(b"CIRF");
    b[4..8].copy_from_slice(&version.to_le_bytes());
    b[8..12].copy_from_slice(&duration.to_le_bytes());
    b[12] = 1;
    b[13..15].copy_from_slice(&128u16.to_le_bytes());
    b[15..19].copy_from_slice(&1704u32.to_le_bytes());
    b[19] = kind;
    b[20] = count;
    b
}

fn feed() -> [u8; 32] {
    request(1, 15, 0, 0)
}

fn frames() -> [u8; 32] {
    request(2, 0, 1, 3)
}

fn ask(socket: &PathBuf, req: &[u8; 32]) -> String {
    let mut s = UnixStream::connect(socket).expect("the daemon is not listening");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(req).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).expect("no reply");
    line.trim_end().to_string()
}

/// A client that hangs up before its reply must cost the daemon an `EPIPE`,
/// not its life. `main` restores the default SIGPIPE disposition for the CLI
/// tools, so the daemon survives only because std's `UnixStream::write` sends
/// with `MSG_NOSIGNAL`. `write_vectored` does not (checked: switching
/// `send_line` to it kills the daemon here). Dying with an auth feed live
/// would skip the torch guard's `Drop` and kill the capture mid-DQBUF, which
/// is the wedge.
#[test]
fn a_client_that_hangs_up_early_does_not_kill_the_daemon() {
    let mut d = spawn("hangup");
    for req in [feed(), frames()] {
        let mut s = UnixStream::connect(&d.socket).unwrap();
        // Shut our read side first: the daemon's reply is then guaranteed to
        // meet a closed reader (EPIPE + SIGPIPE), whatever the scheduling.
        s.shutdown(Shutdown::Read).unwrap();
        s.write_all(&req).unwrap();
        drop(s);
        // Single-threaded: this is served only after the hung-up request.
        let reply = ask(&d.socket, &req);
        assert!(reply.starts_with("error "), "{reply}");
        assert!(d.child.try_wait().unwrap().is_none(), "the daemon exited");
    }
}

/// The daemon runs as this test's own user, so the frames verb lets it
/// through to bring-up, which fails on the missing media node: proof that the
/// v2 request decoded and passed the peer check end to end.
#[test]
fn a_frames_request_from_the_daemons_own_user_reaches_bring_up() {
    let d = spawn("frames");
    let reply = ask(&d.socket, &frames());
    assert!(reply.starts_with("error capture (cannot open "), "{reply}");
    let reply = ask(&d.socket, &request(2, 0, 1, 11));
    assert_eq!(reply, "error invalid (count 11 is outside 1..=10)");
}

/// Frames are biometric data: the daemon runs with no core dumps.
#[test]
fn the_daemon_cannot_dump_core() {
    let d = spawn("core");
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", d.child.id())).unwrap();
    let core = limits.lines().find(|l| l.starts_with("Max core file size")).unwrap();
    let fields: Vec<&str> = core.split_whitespace().collect();
    assert_eq!(&fields[4..6], &["0", "0"], "{core}");
}
