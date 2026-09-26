//! cement-ir-rt — the Rust runtime core of the IR face-auth path.
//!
//! Stood up in slice 013 of the `ir-face-root-cause` milestone; since slice
//! 005 this binary (with `crates/tps68470`) is the emitter's **one owner** —
//! the only code in the repo that writes a PMIC register. `lib/irface.py`,
//! `howdy.sh`, `diag.sh` and `sensor-recover.sh` are consumers that ask it.
//! Since slice 006 it also carries `irfaced` — the on-demand face-capture
//! daemon and its trigger client (see `irfaced.rs`).
//!
//! The verbs:
//! - `torch status` — read back + decode the emitter's PMIC registers
//!   (mirrors `cement-ir torch status`).
//! - `torch hold [--steps N] [--duration S] [--parent PID]` — light the torch
//!   on the verified sequence and hold it on a self-terminating deadline,
//!   enforcing the compiled-in current ceiling (3 steps / 97.5 mA), the
//!   duration ceiling (120 s) and the GSTAT pre-check (refuse on
//!   PWR_FLAG|OVP|UVLO). Release: Drop + signals, PR_SET_PDEATHSIG on the
//!   spawner, and the ~13 s hardware watchdog for SIGKILL.
//! - `torch off` — extinguish (WLEDCTL = 0).
//! - `torch clear-flags` — write-1-to-clear the latched GSTAT bits.
//! - `ip3y unpack` — unpack a 399 360-byte ip3y frame to 16-bit samples.
//! - `irfaced daemon` — run the socket-activated face-capture service.
//! - `irfaced trigger` — the one-shot trigger client (what the PAM helper
//!   calls); always exits 0.
//! - `irfaced resolve` — resolve + configure the graph by entity name and
//!   print it, without streaming (diagnostic).
//! - `soak N` — run N full capture cycles back to back and report wedges,
//!   recoveries and zero-frame cycles (slice 007; see `soak.rs`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

mod capture;
mod faults;
mod irfaced;
mod signals;
mod soak;

use tps68470::bus::Bus;
use tps68470::torch::{self, Torch};
use tps68470::{bus, decode, regs};

#[derive(Parser)]
#[command(name = "cement-ir-rt", version, about = "Rust runtime core of the IR face-auth path")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Light / extinguish / inspect the illuminator (root; /dev/i2c-2).
    Torch {
        #[command(subcommand)]
        cmd: TorchCmd,
    },
    /// Unpack an ip3y raw frame to 16-bit samples (pure; no hardware).
    Ip3y {
        #[command(subcommand)]
        cmd: Ip3yCmd,
    },
    /// The on-demand face-capture service (slice 006) — daemon, trigger
    /// client, or graph-resolution diagnostic.
    Irfaced {
        #[command(subcommand)]
        cmd: IrfacedCmd,
    },
    /// Run N full capture cycles back to back and prove the sensor does not
    /// wedge (slice 007). Exits non-zero if any cycle wedged or needed a
    /// recovery. `cement-ir soak 20` is the slice's bar.
    ///
    /// The illuminator (steps >= 1) and the PMIC state checks need root
    /// (/dev/i2c-2); `--steps 0` soaks the capture + teardown path alone and
    /// runs unprivileged, reporting the gap rather than guessing.
    Soak {
        /// how many cycles to run (20 is the slice's bar)
        #[arg(default_value_t = 20)]
        cycles: u32,
        /// torch current in 32.5 mA steps (0 = no illuminator; 1 = production)
        #[arg(long, default_value_t = irfaced::DEFAULT_STEPS)]
        steps: u8,
        /// capture seconds per cycle — the transitions matter, not the seconds
        #[arg(long, default_value_t = soak::DEFAULT_CYCLE_SECS)]
        duration: u64,
        /// sensor analogue_gain (the production operating point is 128)
        #[arg(long, default_value_t = irfaced::DEFAULT_GAIN)]
        gain: u16,
        /// sensor exposure
        #[arg(long, default_value_t = irfaced::DEFAULT_EXPOSURE)]
        exposure: u32,
        /// the media device
        #[arg(long, default_value = "/dev/media0")]
        media: String,
        /// also republish frames to this loopback (the full production path)
        #[arg(long)]
        loopback: Option<String>,
        /// the fault handler to run after a wedged cycle (the shell entry
        /// point passes lib/sensor-recover.sh; this binary knows no repo path).
        /// Repeatable, last wins — the entry point passes its own first so an
        /// explicit one from the caller overrides it instead of erroring.
        #[arg(long, overrides_with = "recover_cmd")]
        recover_cmd: Option<String>,
        /// settle time between cycles, ms — the rails come down in between
        #[arg(long, default_value_t = soak::DEFAULT_SETTLE_MS)]
        settle_ms: u64,
        /// minimum frames a cycle must produce to count as passing
        #[arg(long, default_value_t = 1)]
        min_frames: u32,
    },
}

#[derive(Subcommand)]
enum IrfacedCmd {
    /// Run the socket-activated daemon (systemd: irfaced.service). Serves
    /// frames to the loopback on demand, keeps the graph configured while
    /// warm, exits after the idle timeout (IRFACED_IDLE_SECS, default 120).
    Daemon,
    /// One-shot trigger: connect, ask for a frame stream, wait for the
    /// readiness reply, print it. Always exits 0 — the PAM helper's contract.
    Trigger {
        /// feed duration in seconds (1..=120; the owner's ceiling bounds it)
        #[arg(long, default_value_t = irfaced::DEFAULT_FEED_SECS)]
        duration: u32,
        /// torch current in 32.5 mA steps (0 = no illuminator; 1..=3)
        #[arg(long, default_value_t = irfaced::DEFAULT_STEPS)]
        steps: u8,
        /// sensor analogue_gain (1..=1023; dlib scored best at 128)
        #[arg(long, default_value_t = irfaced::DEFAULT_GAIN)]
        gain: u16,
        /// sensor exposure
        #[arg(long, default_value_t = irfaced::DEFAULT_EXPOSURE)]
        exposure: u32,
        /// how long to wait for the readiness reply before giving up
        #[arg(long, default_value_t = 5)]
        timeout: u64,
        /// the daemon socket (default /run/cement-ir/irfaced.sock)
        #[arg(long, default_value = irfaced::SOCKET_PATH)]
        socket: String,
    },
    /// Resolve the graph by entity name, configure it, print it — no
    /// streaming. The verification surface for by-name resolution.
    Resolve {
        /// the media device (default /dev/media0)
        #[arg(long, default_value = "/dev/media0")]
        media: String,
    },
}

#[derive(Subcommand)]
enum TorchCmd {
    /// Read back and decode the emitter's PMIC registers.
    Status,
    /// Extinguish the emitter (WLEDCTL = 0). Always allowed.
    Off,
    /// Write-1-to-clear the latched GSTAT fault bits (everything but WAKE).
    ClearFlags,
    /// Light the torch on the verified sequence and hold it.
    ///
    /// Refuses (one line, non-zero exit) above the compiled-in current
    /// ceiling (3 steps / 97.5 mA), above the duration ceiling (120 s), or
    /// while GSTAT latches PWR_FLAG|OVP|UVLO.
    ///
    /// Release: SIGTERM/SIGINT/SIGHUP release immediately (handler -> Drop);
    /// --parent arms PR_SET_PDEATHSIG so the death of the spawning process
    /// (any death, including SIGKILL) releases too; SIGKILL of this process
    /// cannot run code but needs none — the ~13 s hardware watchdog clears
    /// EN by itself. Check with `cement-ir torch status`.
    Hold {
        /// torch current in 32.5 mA steps (1..=3; the owner refuses above)
        #[arg(long, default_value_t = 1)]
        steps: u8,
        /// hold time in seconds (MODE is refreshed inside the 13 s watchdog)
        #[arg(long, default_value_t = 30)]
        duration: u64,
        /// pid to watch: arm PR_SET_PDEATHSIG so this process releases when
        /// the spawner dies (the feeder passes its own pid)
        #[arg(long)]
        parent: Option<u32>,
    },
}

#[derive(Subcommand)]
enum Ip3yCmd {
    /// Unpack a 399 360-byte ip3y frame to 640x480 u16 little-endian.
    Unpack {
        /// the raw frame (the first frame, if the file holds several)
        raw: PathBuf,
        /// output path (614 400 bytes)
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Torch { cmd } => match cmd {
            TorchCmd::Status => torch_status(),
            TorchCmd::Off => torch_off(),
            TorchCmd::ClearFlags => torch_clear_flags(),
            TorchCmd::Hold {
                steps,
                duration,
                parent,
            } => torch_hold(steps, duration, parent),
        },
        Cmd::Ip3y { cmd } => match cmd {
            Ip3yCmd::Unpack { raw, out } => ip3y_unpack(raw, out),
        },
        Cmd::Irfaced { cmd } => match cmd {
            IrfacedCmd::Daemon => irfaced::daemon_run(),
            IrfacedCmd::Trigger { duration, steps, gain, exposure, timeout, socket } => {
                irfaced::trigger_run(duration, steps, gain, exposure, timeout, &socket)
            }
            IrfacedCmd::Resolve { media } => irfaced::resolve_run(&media),
        },
        Cmd::Soak {
            cycles,
            steps,
            duration,
            gain,
            exposure,
            media,
            loopback,
            recover_cmd,
            settle_ms,
            min_frames,
        } => soak::soak_run(soak::Opts {
            cycles,
            steps,
            duration,
            gain,
            exposure,
            media,
            loopback,
            recover_cmd,
            settle_ms,
            min_frames,
        }),
    }
}

fn open_bus() -> Result<bus::Bus, String> {
    bus::Bus::open().map_err(|e| format!("cannot open {}: {e}", bus::BUS))
}

fn get(b: &bus::Bus, reg: u8) -> Option<u8> {
    match b.get(reg) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("cement-ir-rt: read {} failed: {e}", regs::name(reg));
            None
        }
    }
}

fn torch_status() -> ExitCode {
    let b = match open_bus() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cement-ir-rt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (g, s, c, i) = (get(&b, regs::GSTAT), get(&b, regs::WLEDSTAT), get(&b, regs::WLEDCTL), get(&b, regs::ILEDCTL));
    match (g, s, c, i) {
        (Some(g), Some(s), Some(c), Some(i)) => {
            println!("  GSTAT    0x{g:02x}  [{}]", decode::join(&decode::gstat(g)));
            if g & torch::GSTAT_BLOCK_LIGHT != 0 {
                println!(
                    "  (lighting refused while GSTAT latches PWR_FLAG|OVP|UVLO -- sudo cement-ir recover)"
                );
            }
            println!("  WLEDSTAT 0x{s:02x}  [{}]", decode::join(&decode::status(s)));
            let (mode, en, d1, d2, start) = decode::ctl(c);
            println!(
                "  WLEDCTL  0x{c:02x}  [mode={} en={} disled1={} disled2={} start={}]",
                decode::ctl_mode_name(mode),
                en as u8,
                d1 as u8,
                d2 as u8,
                start as u8,
            );
            println!("  ILEDCTL  0x{i:02x}  [{}]", decode::iled(i));
            let rest: Vec<Option<u8>> = [regs::WLEDMAXT, regs::WLEDC1, regs::WLEDC2, regs::VWLEDCTL]
                .iter()
                .map(|&r| get(&b, r))
                .collect();
            if let [Some(m), Some(c1), Some(c2), Some(vw)] = rest.as_slice() {
                println!(
                    "  WLEDMAXT 0x{m:02x}  WLEDC1 0x{c1:02x}  WLEDC2 0x{c2:02x}  VWLEDCTL 0x{vw:02x}"
                );
            }
            ExitCode::SUCCESS
        }
        _ => ExitCode::FAILURE,
    }
}

/// Extinguish the emitter through the owner (the only PMIC writer).
fn torch_off() -> ExitCode {
    let b = match open_bus() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cement-ir-rt: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = torch::off(&b) {
        eprintln!("cement-ir-rt: cannot clear WLEDCTL: {e}");
        return ExitCode::FAILURE;
    }
    match b.get(regs::WLEDCTL) {
        Ok(v) => println!("  torch off (WLEDCTL=0x{v:02x})"),
        Err(e) => {
            eprintln!("cement-ir-rt: readback failed: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// Write-1-to-clear the latched GSTAT bits (everything but WAKE) — the
/// deliberate "clear the latch" surface, replacing `tps68470.py clearflags`.
fn torch_clear_flags() -> ExitCode {
    let b = match open_bus() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cement-ir-rt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let before = match b.get(regs::GSTAT) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cement-ir-rt: read GSTAT failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let after = match torch::clear_gstat(&b) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cement-ir-rt: clear GSTAT failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "  GSTAT before clear 0x{before:02x}  [{}]",
        decode::join(&decode::gstat(before))
    );
    println!(
        "  GSTAT after clear  0x{after:02x}  [{}]",
        decode::join(&decode::gstat(after))
    );
    ExitCode::SUCCESS
}

/// Arm PR_SET_PDEATHSIG so the parent's death (any death — SIGKILL included)
/// arrives here as SIGTERM. Returns false if the parent is already gone.
fn arm_parent_death(parent: u32) -> bool {
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    let ppid = unsafe { libc::getppid() };
    ppid == parent as libc::pid_t
}

/// After the guard released: read GSTAT. A power-fault latch that appeared
/// *during* this hold (the pre-check guaranteed clean at start) means the
/// run browned out the sensor's rail — clear it and report loudly, so the
/// next auth attempt is not refused by a stale latch but the failure is not
/// silent either. SIGKILL skips this code path by nature; the latch then
/// stays and the *next* light attempt refuses + reports it, which is the
/// design working.
fn torch_post_release() -> ExitCode {
    let b = match Bus::open() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cement-ir-rt: post-release check skipped: {e}");
            return ExitCode::SUCCESS;
        }
    };
    match b.get(regs::GSTAT) {
        Ok(g) => {
            println!(
                "  GSTAT after release 0x{g:02x}  [{}]",
                decode::join(&decode::gstat(g))
            );
            if g & torch::GSTAT_BLOCK_LIGHT != 0 {
                match torch::clear_gstat(&b) {
                    Ok(after) => {
                        eprintln!(
                            "cement-ir-rt: GSTAT latched during the hold ({}) -- cleared to 0x{after:02x}; the sensor's rail browned out, run cement-ir recover",
                            decode::join(&decode::gstat(g)),
                        );
                    }
                    Err(e) => {
                        eprintln!("cement-ir-rt: GSTAT latched and the clear failed: {e}");
                    }
                }
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cement-ir-rt: post-release GSTAT read failed: {e}");
            ExitCode::SUCCESS
        }
    }
}

fn torch_hold(steps: u8, duration: u64, parent: Option<u32>) -> ExitCode {
    // Policy checks run before any hardware is touched, so a refusal costs
    // nothing and needs no root.
    if let Err(e) = torch::check_steps(steps) {
        eprintln!("cement-ir-rt: refused: {e}");
        return ExitCode::FAILURE;
    }
    if duration > torch::MAX_DURATION_SECS {
        eprintln!(
            "cement-ir-rt: refused: duration {duration}s is above the ceiling of {}s (the owner's own clock bounds every hold)",
            torch::MAX_DURATION_SECS
        );
        return ExitCode::FAILURE;
    }
    signals::install();
    if let Some(parent) = parent {
        if !arm_parent_death(parent) {
            eprintln!("cement-ir-rt: parent {parent} is already gone; not lighting");
            return ExitCode::FAILURE;
        }
    }
    let b = match open_bus() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cement-ir-rt: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The current ceiling is re-checked inside Torch::on (defence in depth);
    // Torch::on also refuses on a latched GSTAT before any write.
    let mut torch = match Torch::on(b, steps, 4) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cement-ir-rt: refused: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !torch.lit() {
        eprintln!(
            "cement-ir-rt: torch EN never latched after 4 tries (the racy enable; see hardware/notes/005)"
        );
        return ExitCode::FAILURE;
    }
    if signals::SIGNALLED.load(Ordering::SeqCst) {
        // Parent died while the enable sequence ran; release immediately.
        let _ = torch.off();
        eprintln!("cement-ir-rt: signal before the hold started; released");
        return ExitCode::FAILURE;
    }
    println!("torch on: {} x 32.5 mA; holding {} s", steps, duration);
    if parent.is_some() {
        println!("  releasing on SIGTERM/SIGINT/SIGHUP, on parent death, and on the {duration}s deadline; SIGKILL is covered by the ~13 s hardware watchdog");
    } else {
        println!("  SIGTERM/SIGINT/SIGHUP release immediately; SIGKILL is covered by the ~13 s hardware watchdog");
    }
    let start = Instant::now();
    let mut since_refresh = Instant::now();
    // Short-chunk sleep: a SIGTERM (signal or parent death) must be noticed
    // within ~200 ms, not after a full 10 s sleep, or the release promise is
    // hollow. The MODE refresh stays time-based inside the ~13 s watchdog.
    while start.elapsed() < Duration::from_secs(duration) {
        if signals::SIGNALLED.load(Ordering::SeqCst) {
            eprintln!("cement-ir-rt: signal received; releasing");
            break;
        }
        if since_refresh.elapsed() >= Duration::from_secs(10) {
            if let Err(e) = torch.refresh() {
                eprintln!("cement-ir-rt: MODE refresh failed: {e}; releasing");
                break;
            }
            since_refresh = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if let Ok(v) = torch.ctl() {
        println!("releasing; WLEDCTL=0x{v:02x} while lit");
    }
    drop(torch); // clears WLEDCTL now; the post-check reads the released state
    torch_post_release()
}

fn ip3y_unpack(raw: PathBuf, out: PathBuf) -> ExitCode {
    let buf = match std::fs::read(&raw) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cement-ir-rt: cannot read {}: {e}", raw.display());
            return ExitCode::FAILURE;
        }
    };
    if buf.len() < ip3y::FRAME_BYTES {
        eprintln!(
            "cement-ir-rt: {} is {} bytes; an ip3y frame is {}",
            raw.display(),
            buf.len(),
            ip3y::FRAME_BYTES
        );
        return ExitCode::FAILURE;
    }
    let px = ip3y::unpack_frame(&buf);
    let bytes: Vec<u8> = px.iter().flat_map(|v| v.to_le_bytes()).collect();
    if let Err(e) = std::fs::write(&out, &bytes) {
        eprintln!("cement-ir-rt: cannot write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    println!("{} -> {} ({} samples)", raw.display(), out.display(), px.len());
    ExitCode::SUCCESS
}
