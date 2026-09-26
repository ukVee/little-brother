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
//! Release semantics:
//! - `Drop` clears `WLEDCTL` immediately — normal exit and panics.
//! - SIGTERM/SIGINT/SIGHUP: the CLI installs a handler that ends the hold
//!   loop; `Drop` then runs on the normal stack.
//! - SIGKILL: no code can run, and none needs to — the ~13 s hardware
//!   watchdog clears EN by itself. "Released on every exit path" is a
//!   property of the type *and* the hardware together, not a claim about the
//!   code (the same shape the later emitter in slice 005 enforces).
//!
//! No hard current/duration caps and no `OVP|UVLO` refusal here — that is
//! slice 005's work, and it wants this type to exist first.

use std::io;
use std::thread;
use std::time::Duration;

use crate::bus::Bus;
use crate::regs;

pub struct Torch {
    bus: Bus,
    steps: u8,
    lit: bool,
}

impl Torch {
    /// Run the verified enable sequence and return the guard. `steps` is the
    /// current in 32.5 mA units (WLEDMAXT caps at 7). Check `lit()` — the
    /// enable is racy and this does not pretend otherwise.
    pub fn on(bus: Bus, steps: u8, tries: u32) -> Torch {
        let mut t = Torch {
            bus,
            steps,
            lit: false,
        };
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
                    return t;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        t
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
