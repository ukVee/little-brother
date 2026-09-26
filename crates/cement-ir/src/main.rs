//! cement-ir-rt — the Rust runtime core of the IR face-auth path.
//!
//! Skeleton for slice 013 of the `ir-face-root-cause` milestone. Nothing here
//! is on the auth path yet — the shell `./cement-ir` still dispatches
//! everything, and `lib/irface.py` stays the working runtime until later
//! slices cut verbs over one by one
//! (`decision-cement-ir-rust-runtime-core`).
//!
//! The verbs this skeleton carries:
//! - `torch status` — read back + decode the emitter's PMIC registers
//!   (mirrors `cement-ir torch status`).
//! - `torch hold` — light the torch on the verified sequence and hold it,
//!   demonstrating the guard: Drop + signals clear `WLEDCTL` on every exit
//!   path a process can react to, and SIGKILL is covered by the ~13 s
//!   hardware watchdog.
//! - `ip3y unpack` — unpack a 399 360-byte ip3y frame to 16-bit samples.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

mod signals;

use tps68470::{bus, decode, regs, torch::Torch};

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
}

#[derive(Subcommand)]
enum TorchCmd {
    /// Read back and decode the emitter's PMIC registers.
    Status,
    /// Light the torch on the verified sequence and hold it.
    ///
    /// SIGTERM/SIGINT/SIGHUP release immediately (handler -> Drop);
    /// SIGKILL cannot run code but needs none — the ~13 s hardware watchdog
    /// clears EN by itself. Check with `cement-ir torch status`.
    Hold {
        /// torch current in 32.5 mA steps (1..=7; WLEDMAXT caps at 7)
        #[arg(long, default_value_t = 1)]
        steps: u8,
        /// hold time in seconds (MODE is refreshed inside the 13 s watchdog)
        #[arg(long, default_value_t = 30)]
        duration: u64,
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
            TorchCmd::Hold { steps, duration } => torch_hold(steps, duration),
        },
        Cmd::Ip3y { cmd } => match cmd {
            Ip3yCmd::Unpack { raw, out } => ip3y_unpack(raw, out),
        },
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

fn torch_hold(steps: u8, duration: u64) -> ExitCode {
    signals::install();
    let b = match open_bus() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cement-ir-rt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let torch = Torch::on(b, steps, 4);
    if !torch.lit() {
        eprintln!(
            "cement-ir-rt: torch EN never latched after 4 tries (the racy enable; see hardware/notes/005)"
        );
        return ExitCode::FAILURE;
    }
    println!("torch on: {} x 32.5 mA; holding {} s", steps, duration);
    println!("  SIGTERM/SIGINT/SIGHUP release immediately; SIGKILL is covered by the ~13 s hardware watchdog");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(duration) {
        if signals::SIGNALLED.load(Ordering::SeqCst) {
            eprintln!("cement-ir-rt: signal received; releasing");
            break;
        }
        if let Err(e) = torch.refresh() {
            eprintln!("cement-ir-rt: MODE refresh failed: {e}; releasing");
            break;
        }
        std::thread::sleep(Duration::from_secs(10)); // keep inside the ~13 s watchdog
    }
    if let Ok(v) = torch.ctl() {
        println!("releasing; WLEDCTL=0x{v:02x} while lit");
    }
    // Torch drops here -> WLEDCTL = 0x00.
    ExitCode::SUCCESS
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
