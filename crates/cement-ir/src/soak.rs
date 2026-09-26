//! `cement-ir soak N` — run N full capture cycles back to back and prove the
//! sensor does not wedge (slice 007).
//!
//! # Why this exists
//!
//! `sensor-recover.sh` used to run on **every** authentication attempt,
//! before the feeder, with a 25 s timeout, because the sensor "wedges after
//! most feeder runs". A recovery routine on the happy path converts a defect
//! into latency and hides how often the defect fires. Slice 006 took the
//! rebind off the auth path (the daemon's teardown is a clean `STREAMOFF`);
//! this soak is the evidence that doing so was safe, and `cement-ir recover`
//! is now a fault handler that is *counted* when it fires (`faults.rs`).
//!
//! # What one cycle is
//!
//! Exactly what the daemon does per request, through the same code
//! (`capture::bring_up` / `set_operating_point` / `teardown`), at the
//! production operating point (1 step = 32.5 mA, gain 128, exposure 1704):
//!
//! 1. read `GSTAT` (is a power fault already latched?),
//! 2. bring the graph up by entity name,
//! 3. set gain + exposure,
//! 4. `STREAMON` the CIO2 capture — where a wedge shows as `EINVAL`,
//! 5. `STREAMON` the loopback sink, if one was asked for,
//! 6. light the torch through its one owner (skipped at `--steps 0`),
//! 7. pump frames until the per-cycle deadline,
//! 8. tear down in the learned order,
//! 9. check the sensor still answers, `GSTAT` has no new latch, and the torch
//!    really went dark.
//!
//! A cycle that produced no frames, failed a `STREAMON`, lost the sensor, or
//! left a latch is a **wedge**. If `--recover-cmd` was given, the soak then
//! runs the fault handler (which bumps the `recover` counter itself) and keeps
//! going, so the summary can report how many cycles needed one — the number
//! that has to be 0.
//!
//! # Exit status
//!
//! Non-zero if any cycle wedged or needed a recovery. `cement-ir soak 20`
//! passing is slice 007's first Done-when.

use std::process::{Command, ExitCode};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use irctl::video;
use tps68470::bus::Bus;
use tps68470::torch::{self, Torch};
use tps68470::{decode, regs};

use crate::capture::{self, Graph};
use crate::faults;
use crate::signals;

/// Per-cycle capture time. The wedge is a teardown/rail phenomenon, so many
/// short cycles exercise it far harder than one long feed: the point is the
/// number of start/stop transitions, not the seconds of streaming.
pub const DEFAULT_CYCLE_SECS: u64 = 2;
/// Settle time between cycles — the rails come down, `ov7251` powers off.
pub const DEFAULT_SETTLE_MS: u64 = 250;
/// Poll chunk, matching the daemon's (`irfaced::TICK_MS`).
const TICK_MS: i32 = 100;
const NBUFS: u32 = 4;

/// What the PMIC said, when it could be read at all (`/dev/i2c-2` is
/// root-only, so an unprivileged soak reports the gap instead of guessing).
#[derive(Default)]
struct Pmic {
    gstat: Option<u8>,
    wledctl: Option<u8>,
    err: Option<String>,
}

impl Pmic {
    fn read() -> Pmic {
        match Bus::open() {
            Err(e) => Pmic { err: Some(e.to_string()), ..Default::default() },
            Ok(b) => {
                let g = b.get(regs::GSTAT);
                let c = b.get(regs::WLEDCTL);
                let err = match (&g, &c) {
                    (Err(e), _) | (_, Err(e)) => Some(e.to_string()),
                    _ => None,
                };
                Pmic { gstat: g.ok(), wledctl: c.ok(), err }
            }
        }
    }

    /// A power-fault latch on the rail that also feeds the sensor.
    fn latched(&self) -> bool {
        self.gstat.is_some_and(|g| g & torch::GSTAT_BLOCK_LIGHT != 0)
    }

    /// Torch still drawing current after teardown.
    fn torch_on(&self) -> bool {
        self.wledctl.is_some_and(|c| decode::ctl(c).1)
    }

    fn describe(&self) -> String {
        match (self.gstat, self.wledctl) {
            (Some(g), Some(c)) => format!("GSTAT=0x{g:02x} WLEDCTL=0x{c:02x}"),
            _ => format!(
                "pmic=unreadable ({})",
                self.err.as_deref().unwrap_or("no reason given")
            ),
        }
    }
}

/// One cycle's verdict.
struct Cycle {
    frames: u32,
    /// Time from cycle start to the first frame — what the auth path feels.
    first_frame: Option<Duration>,
    /// Empty = the cycle passed.
    faults: Vec<String>,
    /// The cycle hit the wedge signature specifically (vs. a config error).
    wedged: bool,
    /// `GSTAT` latched a power fault when the emitter went out, and the
    /// teardown cleared it. Counted, not faulted: a latch on every single
    /// cycle is the boost's turn-off transient; a latch on some cycles is the
    /// brownout. Only the count over N cycles can tell those apart, which is
    /// the question this soak exists to answer.
    latched_at_release: bool,
    detail: String,
}

impl Cycle {
    fn ok(&self) -> bool {
        self.faults.is_empty()
    }

    fn fault(&mut self, wedged: bool, msg: String) {
        self.wedged |= wedged;
        self.faults.push(msg);
    }
}

pub struct Opts {
    pub cycles: u32,
    pub steps: u8,
    pub duration: u64,
    pub gain: u16,
    pub exposure: u32,
    pub media: String,
    /// Include the loopback hop (the full production path) when given.
    pub loopback: Option<String>,
    /// The fault handler to run after a wedged cycle. The shell entry point
    /// passes `lib/sensor-recover.sh`; nothing in this binary knows a repo
    /// path (the rule that survived the August `mv`).
    pub recover_cmd: Option<String>,
    pub settle_ms: u64,
    pub min_frames: u32,
}

/// Run one cycle. Never panics; every failure lands in the returned `Cycle`.
fn run_cycle(o: &Opts) -> Cycle {
    let mut c = Cycle {
        frames: 0,
        first_frame: None,
        faults: Vec::new(),
        wedged: false,
        latched_at_release: false,
        detail: String::new(),
    };
    let started = Instant::now();

    // 1. Pre-check: a latch left by an earlier cycle means the rail already
    //    browned out, and Torch::on would (correctly) refuse.
    let pre = Pmic::read();
    if pre.latched() {
        c.fault(
            true,
            format!(
                "GSTAT already latched at cycle start ({})",
                decode::join(&decode::gstat(pre.gstat.unwrap_or(0)))
            ),
        );
    }

    // 2. Graph.
    let graph: Graph = match capture::bring_up(std::path::Path::new(&o.media)) {
        Ok(g) => g,
        Err(e) => {
            // `capture::Error` already classified this: a vanished sensor
            // entity or a subdev that will not answer I2C is the wedge's other
            // face, while a missing /dev/media0 is not a wedge at all. Trusting
            // the classification is what keeps the wedge count meaningful.
            c.fault(e.wedged, format!("bring-up failed: {}", e.msg));
            c.detail = pre.describe();
            return c;
        }
    };

    // 3. Operating point, immediately before capture, every time.
    capture::set_operating_point("soak", &graph, o.gain, o.exposure);

    // 4. CIO2 capture STREAMON — the wedge's EINVAL lands here.
    let mut cap = match video::Stream::start(
        graph.video.to_str().unwrap_or_default(),
        true,
        &video::ip3y_640x480(),
        NBUFS,
    ) {
        Ok(s) => s,
        Err(e) => {
            let wedged = faults::looks_wedged(&e);
            c.fault(wedged, format!("capture {}: {e}", graph.video.display()));
            c.detail = format!("{}; {}", pre.describe(), capture::readback(&graph).describe());
            return c;
        }
    };

    // 5. Loopback sink, if the full production path was asked for.
    let mut sink = match &o.loopback {
        None => None,
        Some(dev) => match video::Stream::start(dev, false, &video::grey_640x480(), NBUFS) {
            Ok(s) => Some(s),
            Err(e) => {
                c.fault(false, format!("loopback {dev}: {e}"));
                let t = capture::teardown("soak", None, &mut cap, None, &graph);
                c.detail = format!("{}; {}", pre.describe(), t.readback.describe());
                return c;
            }
        },
    };

    // 6. The illuminator, through its one owner (in-process guard: Drop
    //    clears WLEDCTL even if this function returns early).
    let torch = if o.steps > 0 {
        match Bus::open().map_err(|e| e.to_string()).and_then(|b| {
            Torch::on(b, o.steps, 4).map_err(|e| e.to_string())
        }) {
            Ok(t) => Some(t),
            Err(e) => {
                c.fault(false, format!("torch refused: {e}"));
                let t = capture::teardown("soak", None, &mut cap, sink.as_mut(), &graph);
                c.detail = format!("{}; {}", pre.describe(), t.readback.describe());
                return c;
            }
        }
    } else {
        None
    };

    // 7. Pump frames to the deadline.
    let deadline = Instant::now() + Duration::from_secs(o.duration);
    let mut since_refresh = Instant::now();
    let mut pump_err: Option<String> = None;
    while Instant::now() < deadline {
        if signals::SIGNALLED.load(Ordering::SeqCst) {
            break;
        }
        if let Some(t) = &torch {
            // MODE refresh inside the ~13 s hardware watchdog.
            if since_refresh.elapsed() >= Duration::from_secs(10) {
                if let Err(e) = t.refresh() {
                    pump_err = Some(format!("MODE refresh failed: {e}"));
                    break;
                }
                since_refresh = Instant::now();
            }
        }
        let (frames, first_frame) = (&mut c.frames, &mut c.first_frame);
        let sink_ref = sink.as_mut();
        let r = cap.with_frame(TICK_MS, |frame| {
            let mut px = vec![0u16; ip3y::PIXELS];
            ip3y::unpack(frame, &mut px);
            if let Some(s) = sink_ref {
                // 10-bit -> 8-bit, exactly what the daemon feeds the loopback.
                let grey: Vec<u8> = px.iter().map(|v| (v >> 2) as u8).collect();
                s.write_frame(&grey, TICK_MS)?;
            }
            *frames += 1;
            if first_frame.is_none() {
                *first_frame = Some(started.elapsed());
            }
            Ok(())
        });
        if let Err(e) = r {
            pump_err = Some(format!("capture error after {} frames: {e}", c.frames));
            if faults::looks_wedged(&e) {
                c.wedged = true;
            }
            break;
        }
    }
    if let Some(e) = pump_err {
        c.fault(c.wedged, e);
    }

    // 8. Teardown in the learned order — the mechanism under test.
    let t = capture::teardown("soak", torch, &mut cap, sink.as_mut(), &graph);
    c.latched_at_release = t.post_release.as_ref().is_some_and(|p| p.latched());
    for e in &t.errors {
        // A failed STREAMOFF is itself the wedge's shape.
        c.fault(true, format!("teardown: {e}"));
    }

    // 9. Post-conditions.
    if c.frames < o.min_frames {
        c.fault(
            true,
            format!("{} frames (need >= {})", c.frames, o.min_frames),
        );
    }
    if !t.readback.answered() {
        c.fault(
            true,
            format!("sensor stopped answering: {}", t.readback.describe()),
        );
    } else if t.readback.gain != Some(o.gain as i32) {
        // Not a wedge — the rail dropped and the sensor re-defaulted (gain 16
        // = near-black frames that look identical to a dark torch).
        c.fault(
            false,
            format!(
                "gain reverted: set {} read {:?} (a re-powered sensor; frames were near-black)",
                o.gain, t.readback.gain
            ),
        );
    }
    let post = Pmic::read();
    if post.latched() {
        c.fault(
            true,
            format!(
                "GSTAT latched during the cycle ({})",
                decode::join(&decode::gstat(post.gstat.unwrap_or(0)))
            ),
        );
    }
    if post.torch_on() {
        c.fault(false, "torch still EN after teardown".to_string());
    }
    c.detail = format!("{}; {}", post.describe(), t.readback.describe());
    c
}

/// Run the fault handler after a wedged cycle. It bumps the `recover` counter
/// itself (it is the place the rebind happens), so this only reports.
///
/// Executed directly (not through `bash`): the shipped handler is an
/// executable script with its own shebang, and running it directly keeps the
/// flag useful for anything else the caller wants to hand it.
fn run_recovery(cmd: &str) -> bool {
    eprintln!("soak: running the fault handler: {cmd}");
    match Command::new(cmd).status() {
        Ok(st) if st.success() => {
            eprintln!("soak: fault handler recovered the sensor");
            true
        }
        Ok(st) => {
            eprintln!("soak: fault handler exited {st} — the sensor is still wedged");
            false
        }
        Err(e) => {
            eprintln!("soak: cannot run the fault handler {cmd}: {e}");
            false
        }
    }
}

pub fn soak_run(o: Opts) -> ExitCode {
    if o.cycles == 0 {
        eprintln!("soak: 0 cycles asked for; nothing to prove");
        return ExitCode::FAILURE;
    }
    if let Err(e) = torch::check_steps(o.steps).map_err(|e| e.to_string()) {
        // steps 0 is legal here (a dark soak: the capture path without the
        // emitter — the only shape an unprivileged run can take).
        if o.steps != 0 {
            eprintln!("soak: refused: {e}");
            return ExitCode::FAILURE;
        }
    }
    signals::install();

    let recover_before = faults::get(faults::RECOVER);
    let wedge_before = faults::get(faults::WEDGE);
    println!(
        "soak: {} cycles x {}s, {} steps ({:.1} mA), gain {}, exposure {}{}",
        o.cycles,
        o.duration,
        o.steps,
        f64::from(o.steps) * 32.5,
        o.gain,
        o.exposure,
        match &o.loopback {
            Some(d) => format!(", loopback {d}"),
            None => " (no loopback hop)".to_string(),
        },
    );
    if o.steps == 0 {
        println!("soak: NOTE steps=0 — the illuminator is not exercised; this soaks the capture/teardown path only");
    }
    println!(
        "soak: counters before: recover={recover_before} wedge={wedge_before} ({})",
        faults::path()
    );

    let started = Instant::now();
    let mut ok = 0u32;
    let mut wedges = 0u32;
    let mut latched = 0u32;
    let mut recoveries = 0u32;
    let mut zero_frame = 0u32;
    let mut total_frames = 0u64;
    let mut min_frames = u32::MAX;
    let mut worst_first: Option<Duration> = None;
    let mut ran = 0u32;

    for i in 1..=o.cycles {
        if signals::SIGNALLED.load(Ordering::SeqCst) {
            println!("soak: signal received; stopping after {ran} cycles");
            break;
        }
        ran += 1;
        let c = run_cycle(&o);
        total_frames += u64::from(c.frames);
        min_frames = min_frames.min(c.frames);
        if c.frames == 0 {
            zero_frame += 1;
        }
        if let Some(d) = c.first_frame {
            if worst_first.is_none_or(|w| d > w) {
                worst_first = Some(d);
            }
        }
        let first = c
            .first_frame
            .map(|d| format!("{} ms", d.as_millis()))
            .unwrap_or_else(|| "never".to_string());
        if c.latched_at_release {
            latched += 1;
        }
        if c.ok() {
            ok += 1;
            println!(
                "  cycle {i:>3}/{}  ok     frames={:<4} first={first:<8} {}",
                o.cycles, c.frames, c.detail
            );
        } else {
            if c.wedged {
                wedges += 1;
                faults::record(
                    faults::WEDGE,
                    &format!("soak cycle {i}: {}", c.faults.join("; ")),
                );
            }
            println!(
                "  cycle {i:>3}/{}  FAULT  frames={:<4} first={first:<8} {}",
                o.cycles, c.frames, c.detail
            );
            for f in &c.faults {
                println!("        - {f}");
            }
            // The handler is a fault handler now: it runs *here*, after a real
            // failure, never in front of an auth attempt.
            if c.wedged {
                if let Some(cmd) = &o.recover_cmd {
                    recoveries += 1;
                    run_recovery(cmd);
                } else {
                    println!("        (no --recover-cmd; run `sudo cement-ir recover` by hand)");
                }
            }
        }
        if o.settle_ms > 0 && i < o.cycles {
            std::thread::sleep(Duration::from_millis(o.settle_ms));
        }
    }

    let elapsed = started.elapsed();
    let recover_after = faults::get(faults::RECOVER);
    let wedge_after = faults::get(faults::WEDGE);
    if min_frames == u32::MAX {
        min_frames = 0;
    }
    // One summary line, the thing that gets pasted into the handoff.
    println!(
        "soak: {ran} cycles, {ok} ok, {wedges} wedges, {recoveries} recoveries, {zero_frame} zero-frame, {latched} latched-at-release; frames {total_frames} (min {min_frames}/cycle); slowest first frame {}; {:.1} s",
        worst_first
            .map(|d| format!("{} ms", d.as_millis()))
            .unwrap_or_else(|| "n/a".to_string()),
        elapsed.as_secs_f64(),
    );
    println!(
        "soak: counters after: recover={recover_after} (+{}) wedge={wedge_after} (+{})",
        recover_after - recover_before,
        wedge_after - wedge_before,
    );
    if latched > 0 {
        println!(
            "soak: GSTAT latched a power fault at release on {latched}/{ran} cycles (the teardown cleared each one). {}",
            if latched == ran {
                "EVERY cycle -- that is the boost's turn-off transient, not a brownout, and both the refusal wording and hardware/notes/005's '130 mA x 40 s' framing need correcting."
            } else {
                "SOME cycles -- that is the brownout signature: the emitter is stressing the rail the sensor shares."
            }
        );
    }
    if ok == ran && ran == o.cycles {
        println!("soak: PASS — no wedge, no recovery, no zero-frame cycle");
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "soak: FAIL — {} of {ran} cycles faulted ({wedges} wedged, {recoveries} recovered)",
            ran - ok
        );
        ExitCode::FAILURE
    }
}
