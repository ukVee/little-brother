//! The torch guard — the emitter has exactly one owner, and releases on
//! every exit path.
//!
//! The enable sequence is load-bearing (`hardware/notes/005`): disable, let
//! the boost ramp down, write `WLEDMAXT`/`WLEDC1`/`WLEDC2`, enable the boost,
//! write MODE **first**, then EN as a **separate** write, then *verify EN
//! latched and retry* — the naive single write failed 3 times in 16 captures.
//! The torch also self-disables after a ~13 s hardware watchdog unless MODE
//! is re-written (`refresh`).
//!
//! Since slice 005 this crate is the emitter's **one owner** — the only code
//! in the repo that writes a PMIC register. It now enforces, instead of
//! trusting callers to respect:
//!
//! - a hard **current ceiling** ([`MAX_STEPS`], 97.5 mA — one 32.5 mA step
//!   under the measured damage current of 130 mA that latched
//!   `PWR_FLAG|OVP|UVLO` in August 2026),
//! - a hard **duration ceiling** ([`MAX_DURATION_SECS`], enforced by the
//!   holder's own clock in the `cement-ir-rt` binary, so a caller that never
//!   reaches its cleanup line cannot strand the emitter on),
//! - a **GSTAT pre-check**: refuse to light while `PWR_FLAG|OVP|UVLO` is
//!   latched (that is a wedged sensor; the frame would be useless), and a
//!   **post-release check** that clears + reports a latch the run itself
//!   caused (done by the holder verb; see `crates/cement-ir`).
//!
//! Release semantics:
//! - `Drop` clears `WLEDCTL` immediately — normal exit and panics.
//! - SIGTERM/SIGINT/SIGHUP: the CLI installs a handler that ends the hold
//!   loop; `Drop` then runs on the normal stack.
//! - Parent death: the holder verb arms `PR_SET_PDEATHSIG`, so `kill -9` of
//!   the feeder (or its parent) turns into a SIGTERM here — released, not
//!   orphaned.
//! - SIGKILL: no code can run, and none needs to — the ~13 s hardware
//!   watchdog clears EN by itself. "Released on every exit path" is a
//!   property of the type *and* the hardware together, not a claim about the
//!   code.

use std::fmt;
use std::io;
use std::thread;
use std::time::Duration;

use crate::bus::Bus;
use crate::decode;
use crate::regs;

/// Compiled-in current ceiling: 3 steps = 97.5 mA. The production operating
/// point is 1 step; the measured damage point is 4 steps (130 mA) held 40 s
/// (`hardware/notes/005`). A caller cannot exceed this — `irface.py`'s
/// `--current` default of 1 is a *default*, and defaults are not limits.
pub const MAX_STEPS: u8 = 3;

/// Compiled-in duration ceiling in seconds. Enrolment (`howdy.sh`) feeds for
/// 90 s at 1 step and needs the margin; any hold is bounded by the owner's
/// own clock regardless of the caller's behaviour. Enforced by the
/// `cement-ir-rt` hold verb.
pub const MAX_DURATION_SECS: u64 = 120;

/// GSTAT bits that make lighting unsafe: `PWR_FLAG|OVP|UVLO` — an
/// undervoltage/overvoltage latch on the very rail that powers the sensor.
/// (A `SHORT_FLAG` alone does not block lighting: the slice scopes the
/// refusal to the power-fault class that wedges `ov7251`.)
pub const GSTAT_BLOCK_LIGHT: u8 = 0x4C;

#[derive(Debug)]
pub enum Error {
    /// The requested current is outside 1..=MAX_STEPS (0 means "use off";
    /// anything above the ceiling is refused, not clamped).
    CurrentCap { requested: u8 },
    /// `GSTAT` already has a power-fault latch; lighting is refused.
    Latched { gstat: u8 },
    /// The bus itself failed (the pre-check read could not run — fail
    /// closed, never light blind).
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CurrentCap { requested } => {
                if *requested == 0 {
                    write!(f, "current 0 steps means off -- use 'torch off'")
                } else {
                    write!(
                        f,
                        "current {} steps ({:.1} mA) is above the ceiling of {} steps ({:.1} mA)",
                        requested,
                        f64::from(*requested) * 32.5,
                        MAX_STEPS,
                        f64::from(MAX_STEPS) * 32.5,
                    )
                }
            }
            Error::Latched { gstat } => write!(
                f,
                "GSTAT latched {} (0x{gstat:02x}) -- the sensor's rail may be wedged; run cement-ir recover",
                decode::join(&decode::gstat(*gstat)),
            ),
            Error::Io(e) => write!(f, "PMIC bus error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Pure policy check: the current must be within 1..=MAX_STEPS.
pub fn check_steps(steps: u8) -> Result<(), Error> {
    if steps == 0 || steps > MAX_STEPS {
        return Err(Error::CurrentCap { requested: steps });
    }
    Ok(())
}

/// Pure policy check: refuse to light with a power-fault latch present.
pub fn check_gstat(v: u8) -> Result<(), Error> {
    if v & GSTAT_BLOCK_LIGHT != 0 {
        return Err(Error::Latched { gstat: v });
    }
    Ok(())
}

/// Extinguish the emitter. Always allowed — the refusal guards *lighting*.
pub fn off(bus: &Bus) -> io::Result<()> {
    bus.put(regs::WLEDCTL, 0x00)
}

/// Write-1-to-clear the latched GSTAT bits (everything but WAKE, matching
/// the old `tps68470.py clearflags`). Returns the post-clear value.
pub fn clear_gstat(bus: &Bus) -> io::Result<u8> {
    bus.put(regs::GSTAT, 0xFE)?;
    bus.get(regs::GSTAT)
}

/// What a post-release `GSTAT` read found — see [`post_release`].
#[derive(Debug, Default)]
pub struct PostRelease {
    /// `GSTAT` as read after the emitter went out.
    pub gstat: u8,
    /// `Some(after)` when a power-fault latch was present and cleared.
    pub cleared: Option<u8>,
    /// Why the check could not be taken, if it could not.
    pub err: Option<String>,
}

impl PostRelease {
    /// A power-fault latch was present when the emitter went out.
    pub fn latched(&self) -> bool {
        self.gstat & GSTAT_BLOCK_LIGHT != 0
    }

    pub fn describe(&self) -> String {
        if let Some(e) = &self.err {
            return format!("post-release GSTAT unreadable: {e}");
        }
        let bits = crate::decode::join(&crate::decode::gstat(self.gstat));
        match self.cleared {
            Some(after) => format!("GSTAT 0x{:02x} [{bits}] -> cleared to 0x{after:02x}", self.gstat),
            None => format!("GSTAT 0x{:02x} [{bits}]", self.gstat),
        }
    }
}

/// Read `GSTAT` once the emitter is out, and clear a power-fault latch.
///
/// **One implementation for every path that lights the emitter.** It used to
/// live only in the `torch hold` CLI, so the daemon lit, released, latched
/// `OVP` and left it standing — and the *next* auth's pre-check refused on
/// that stale latch. Face auth therefore worked about every other attempt
/// (observed end to end on 2026-09-26, D3/D9). The pre-check stays strict
/// precisely because this runs: after it, a latch that is still present is
/// one no normal release produced.
pub fn post_release(bus: &Bus) -> PostRelease {
    let gstat = match bus.get(regs::GSTAT) {
        Ok(g) => g,
        Err(e) => return PostRelease { err: Some(e.to_string()), ..Default::default() },
    };
    if gstat & GSTAT_BLOCK_LIGHT == 0 {
        return PostRelease { gstat, ..Default::default() };
    }
    match clear_gstat(bus) {
        Ok(after) => PostRelease { gstat, cleared: Some(after), err: None },
        Err(e) => PostRelease { gstat, cleared: None, err: Some(format!("clear failed: {e}")) },
    }
}

pub struct Torch {
    bus: Bus,
    steps: u8,
    lit: bool,
}

impl Torch {
    /// Run the verified enable sequence and return the guard. `steps` is the
    /// current in 32.5 mA units. Refuses (before writing anything) when the
    /// request exceeds [`MAX_STEPS`] or when `GSTAT` already latches
    /// `PWR_FLAG|OVP|UVLO`. Check `lit()` — the enable is racy and this does
    /// not pretend otherwise.
    pub fn on(bus: Bus, steps: u8, tries: u32) -> Result<Torch, Error> {
        check_steps(steps)?;
        let mut t = Torch {
            bus,
            steps,
            lit: false,
        };
        check_gstat(t.bus.get(regs::GSTAT)?)?; // fail closed: no read, no light
        for _ in 0..tries {
            let _ = t.bus.put(regs::WLEDCTL, 0x00); // MAXT is not writable while enabled
            thread::sleep(Duration::from_millis(20)); // let the boost ramp down
            let _ = t.bus.put(regs::WLEDMAXT, steps);
            let _ = t.bus.put(regs::WLEDC1, steps);
            let _ = t.bus.put(regs::WLEDC2, steps);
            let _ = t.bus.put(regs::VWLEDCTL, 0x39); // reset value | ENABLE
            let _ = t.bus.put(regs::WLEDCTL, 0x01); // MODE='01' torch FIRST...
            thread::sleep(Duration::from_millis(10));
            let _ = t.bus.put(regs::WLEDCTL, 0x05); // ...THEN EN, separate write
            if let Ok(v) = t.bus.get(regs::WLEDCTL) {
                if v & 0x04 != 0 {
                    // EN actually latched
                    t.lit = true;
                    return Ok(t);
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok(t)
    }

    /// The bus this guard holds — so a caller can take the post-release
    /// reading without opening `/dev/i2c-2` a second time.
    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    pub fn lit(&self) -> bool {
        self.lit
    }

    pub fn steps(&self) -> u8 {
        self.steps
    }

    /// Read back `WLEDCTL` as the hardware holds it.
    pub fn ctl(&self) -> io::Result<u8> {
        self.bus.get(regs::WLEDCTL)
    }

    /// Re-write MODE+EN inside the ~13 s hardware watchdog
    /// (`torch_refresh()` in `irface.py`).
    pub fn refresh(&self) -> io::Result<()> {
        self.bus.put(regs::WLEDCTL, 0x01)?;
        self.bus.put(regs::WLEDCTL, 0x05)?;
        Ok(())
    }

    /// Explicit release. `Drop` does the same.
    pub fn off(&mut self) -> io::Result<()> {
        self.lit = false;
        self.bus.put(regs::WLEDCTL, 0x00)
    }
}

impl Drop for Torch {
    fn drop(&mut self) {
        if self.lit {
            let _ = self.bus.put(regs::WLEDCTL, 0x00);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_ceiling_refuses_above_and_zero() {
        for ok in 1..=MAX_STEPS {
            assert!(check_steps(ok).is_ok(), "{ok} steps must be allowed");
        }
        for bad in [0, MAX_STEPS + 1, 7, 255] {
            let e = check_steps(bad).unwrap_err();
            assert!(matches!(e, Error::CurrentCap { .. }), "{bad} steps must refuse");
        }
    }

    #[test]
    fn gstat_power_fault_refuses_lighting() {
        for bad in [0x04, 0x08, 0x40, 0x4C, 0x6C, 0xFF] {
            assert!(
                matches!(check_gstat(bad), Err(Error::Latched { .. })),
                "GSTAT 0x{bad:02x} must refuse lighting"
            );
        }
    }

    #[test]
    fn gstat_clean_or_non_power_faults_do_not_refuse() {
        for ok in [0x00, 0x01, 0x02, 0x10, 0x80, 0x81, 0xB2] {
            assert!(check_gstat(ok).is_ok(), "GSTAT 0x{ok:02x} must not refuse");
        }
    }

    #[test]
    fn block_light_mask_is_exactly_pwr_ovp_uvlo() {
        assert_eq!(GSTAT_BLOCK_LIGHT, 0x40 | 0x08 | 0x04);
    }
}
